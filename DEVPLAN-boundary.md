# Crate boundaries — make `gix-anchor` storage-free

`DEVPLAN-storage.md` moved the storage *engine* out of `gix-anchor` and into `gix-store`.
This plan moves the storage *concern* out too.

`gix-anchor/src/store.rs` still owns a document — `Note { body, binding, attachment, parent, state, created_at }` — and a `Store` over it.
Three of those fields are documented in that file as opaque passthrough: `gix-anchor` never reads `parent` or `state`, and carries them only so `gix-comment` can build reply threads and a lifecycle.
A primitive is holding a consumer's domain shape.

Goal: `gix-anchor` is types and pure functions — `Anchor`, `Binding`, `LineRange`, `Oid`, `capture`, `project`, `revalidate`, `diff_trees` — with **no storage dependency**.
`gix-comment` owns its own document and drives `gix_store::Kind` directly.
See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the rule this follows and the alternatives it rejects.

"No storage dependency" means no `gix-store` and no `gix-refstore`.
`facet-git-tree` stays: it is a codec, not storage, and `Binding`'s `Facet` derive plus `serialize_into`/`deserialize` are pure encoding over a caller-supplied object sink.

**No back-compat constraint.**
Same standing as `DEVPLAN-storage.md`: nothing outside this repo family consumes any of these APIs, so ref layout, public Rust API, and CLI surface all change freely where that produces the better design.

## Phase 1 — Embed `Binding` inline, not by tree id

`Note.binding` is a `RawTree` today: an opaque oid pointing at the binding's serialized tree.
That keeps `Binding`'s shape *out* of the document's published schema, which forfeits the property that makes `Binding` worth having as a vocabulary type — a generic consumer cannot discover by reflection that a kind is anchorable.

Change the field to an inline `Binding`.
`facet-git-tree` already handles the externally-tagged enum, so this is a field-type change, not a codec change.
Retention is unaffected: an inline binding's content and context blobs are ordinary entries in the document's own tree, still reachable, still never gitlinks.

Two consequences to settle while writing it:

1. **Binding-keyed identity.**
   `Store::attach` names an entity by the binding's serialized tree oid.
   With the binding inline, that oid is a subtree of the document rather than a separately written root.
   `Binding::serialize_into` remains available for deriving it (a pure codec call), so binding-keyed naming stays possible — but it is now a *consumer convention*, not a storage feature, and `gix-comment` uses genesis identity exclusively.
   Decide in Phase 2 whether anything still needs it.
2. **Schema churn.**
   The published schema changes shape, so previously written entities do not deserialize.
   There is no data to preserve; do not write a migration.

## Phase 2 — Move the document into `gix-comment`

`gix-comment` gains a `gix-store` dependency and defines its own document:

```rust
#[derive(Facet)]
struct Comment { body: String, binding: Binding, parent: Option<String>, state: Option<String>, created_at: u64 }
```

Everything in `gix-anchor/src/store.rs` that is not `Note` itself moves with it, renamed to the domain: the `Layout` split (`refs/comments/{data,schema}`), the kind segment, `NoteName` → an entity-name type over `<target-hex>/<id-hex>`, `published()`'s check-then-publish, `find()`'s scan-by-id, `now_nanos`.

`Comments`' public API — `add`, `reply`, `thread`, `resolve`, `reopen`, `get`, `get_at`, `list`, `history`, `remove` — does not change shape.
Its `hydrate` stops translating from `StoredNote` and reads its own document directly, which removes one whole layer of field-by-field copying.

Delete from `gix-anchor`: `src/store.rs`, the `store::{RepoStore, Store, StoredNote}` re-exports, the `RefPrefix` re-export, `From<gix_store::Error> for Error`, and the `gix-store` dependency.

Port `store.rs`'s test module rather than rewriting it.
`FlakyRefStore`, `SplitIdentity`, and the CAS-retry tests are testing `gix-store`'s retry behavior through a consumer, which is still worth doing — from `gix-comment` now.

## Phase 3 — Decide what `git anchor` is

**This is the open decision that needs an answer before Phase 2 lands**, because `git anchor add/list/show/remove` are storage commands over the `Store` that Phase 2 deletes.

- **(a) Projection-only CLI (recommended).**
  `git anchor` keeps `capture`, `project`, and a tree-pair `diff`, writes no refs, and stores nothing.
  `git comment` is the tool that attaches content and reads it back — it already does exactly that.
  Cost: deletes `add`, `list`, `remove`, and their integration tests; the CLI becomes an inspection tool for the primitive.
- **(b) The CLI defines its own document.**
  A binary is an application, so it may legitimately define an anchor-note document over `gix-store` and keep every subcommand.
  Cost: two near-identical documents in one repo, which is the smell this plan exists to remove.

Recommendation is (a).
`git anchor add` and `git comment add` differ only in which ref namespace they write, and that is not a difference worth a second document.

## Phase 4 — Docs

- `crates/gix-anchor/README.md` — delete the "Store: notes attached to objects" section; state that persistence is the consumer's, over `gix-store`.
- `crates/gix-comment/README.md` — drop "This crate adds no persistence of its own"; it now owns its persistence.
- `crates/git-anchor/README.md`, `crates/git-comment/README.md`, root `README.md` — whatever Phase 3 decides about the CLI.
- `gix-anchor/src/lib.rs` module doc — the spec-coverage list stays accurate, but the crate is no longer where a consumer's document lives.
- `DEVPLAN-attest.md` — a claim is a document with a `binding: Binding` field, owned by `gix-attest`, over its own `gix-store` layout.
  Its Phase 2 ref-layout question is narrowed by that, and its Phase 3/4 must not plan to reuse `gix_anchor::Store`, which will not exist.

## Definition of done

- `crates/gix-anchor/Cargo.toml` names neither `gix-store` nor `gix-refstore`.
- `grep -rn 'refs/' crates/gix-anchor/src` finds nothing.
- `crates/gix-anchor/src/store.rs` does not exist.
- `gix-comment`'s published schema records `Binding`'s shape, with a test that locates the binding field in a schema read back from the registry — the anchorable-by-reflection property, asserted rather than assumed.
- `cargo test --workspace` passes, doctests included.
- No shim, no re-export of the moved types from `gix-anchor` for convenience.
