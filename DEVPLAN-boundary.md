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
This constraint is on the `gix-anchor` *library*, not on applications built over it: `gix-comment` (Phase 2) and the `git-anchor` binary (Phase 3) both take on `gix-store` directly, which is exactly what a consumer crate — as opposed to the primitive — is for.

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
struct Comment { body: String, binding: Binding, parent: Option<String>, state: Option<State>, created_at: u64 }
```

`state` is `Option<State>`, not `Option<String>` — `State` (`crates/gix-comment/src/comment.rs`) gains a `#[derive(Facet)]` and becomes the wire representation directly, rather than a `String` translated at the app layer.
Phase 3 works out why: a schema-level enum is what makes an unrecognized state value a write-time failure for every writer, not just `gix-comment`'s own.

Everything in `gix-anchor/src/store.rs` that is not `Note` itself moves with it, renamed to the domain: the `Layout` split (`refs/comments/{data,schema}`), the kind segment, `NoteName` → an entity-name type over `<target-hex>/<id-hex>`, `published()`'s check-then-publish, `find()`'s scan-by-id, `now_nanos`.

`Comments`' public API — `add`, `reply`, `thread`, `resolve`, `reopen`, `get`, `get_at`, `list`, `history`, `remove` — does not change shape.
Its `hydrate` stops translating from `StoredNote` and reads its own document directly, which removes one whole layer of field-by-field copying.

Delete from `gix-anchor`: `src/store.rs`, the `store::{RepoStore, Store, StoredNote}` re-exports, the `RefPrefix` re-export, `From<gix_store::Error> for Error`, and the `gix-store` dependency.

Port `store.rs`'s test module rather than rewriting it.
`FlakyRefStore`, `SplitIdentity`, and the CAS-retry tests are testing `gix-store`'s retry behavior through a consumer, which is still worth doing — from `gix-comment` now.

## Phase 3 — `git anchor` is generic over registered schemas

`git anchor add/list/show/remove` stop being a hand-rolled client of the `Store` Phase 2 deletes and become a thin driver over whatever kind the caller names.
`git anchor add comment "some text"` is `git comment add "some text"` by another name — same write, same ref namespace, same schema — because `git anchor` reads `comment`'s published schema out of the registry and writes an entity of it without ever having been compiled against `gix_comment::Comment`.
`git comment` does not go away: it stays the ergonomic, domain-validating front end for its own kind, and is what most users type.
`git anchor` is the tool that works for a kind it has never heard of — the concrete proof of the reflection property `ARCHITECTURE.md` claims for `Binding`, not an inspection tool for the primitive or a second document maintained by hand.

Two narrower answers were considered and rejected: a projection-only CLI (`capture`/`project`/`diff`, no storage at all) deletes `add`/`list`/`remove` outright, so the CLI stops being able to create anything; the CLI defining its own document buys `add`/`list`/`remove` back at the price of a second near-identical document, the exact smell this plan exists to remove.
Generic-over-schemas gets `add`/`list`/`remove` back without either cost, because it defines no document of its own at all.

### The substrate this requires

`gix-store` already has everything `git anchor` needs to be a schema-generic writer; none of it is new:

- **Enumerate registered kinds.**
  `Store::kinds() -> Result<Vec<RefSegment>, Error>` (`crates/gix-store/src/store.rs`) lists every kind with a published schema — `git anchor` with no kind name lists these, `git anchor add <kind>` resolves one by name.
- **A handle with no compile-time type.**
  `Store::dynamic(name: RefSegment) -> Kind<'_, Dynamic, R, O>` (same file) is `Store::kind::<T>`'s untyped sibling.
  `Dynamic` (`crates/gix-store/src/encoding.rs`) sets `Encoding::Value = facet_value::Value` — a self-describing dynamic value, not a `Facet`-derived Rust type — and its `write`/`read` are `serialize_value_with_schema`/`deserialize_value_with_schema` (`crates/facet-git-tree/src/schema/{write,read}.rs`), both driven entirely by a `Schema` fetched at runtime rather than by `T::SHAPE`.
  It round-trips through `facet_value::Value` exactly: those two functions are `Dynamic`'s only encode/decode paths, and `serialize_value_with_schema`'s own doc example asserts its output is byte-identical to the typed encoding of equivalent data, so a dynamic write and a `Typed<Comment>` write of the same content land at the same object id.
  A caller needs two things in hand to write: the kind's `Schema` (next) and a `facet_value::Value` shaped to match it, built directly — `facet_value`'s `value!` macro, or `VObject`/`VArray` by hand — never through a `#[derive(Facet)]` type.
- **Fetch a kind's schema.**
  `Kind::schema()` then `KindSchema::get() -> Result<Option<Schema>, Error>` (`crates/gix-store/src/kind.rs`) returns the published `Schema { root: Node, defs: BTreeMap<String, Node> }` (`crates/facet-git-tree/src/schema/mod.rs`).

### Locating the binding field by reflection

`Schema` and `Node` both derive `PartialEq`, which is what makes "does this schema embed `Binding`'s shape" answerable by inspection rather than by convention.
Resolve the fetched schema's `root` (through one `Node::Ref` indirection into `defs`, for any named struct) to a `Node::Struct(BTreeMap<String, Node>)`; a kind is anchorable iff one of that map's fields, itself resolved through the *same schema's* `defs`, is structurally equal to `schema_of::<gix_anchor::Binding>()?`'s own root definition — concretely, `doc.defs.get("Binding") == Some(&canonical.defs["Binding"])` where `canonical = schema_of::<Binding>()?`.
Both sides are ordinary `Node` values, so this is `==`, not a hand-written shape-walker.
This is the check `ARCHITECTURE.md`'s reflection paragraph now points `git anchor add <kind>` at, and the assertion Phase 2's definition of done already commits `gix-comment` to have a test for.

### Consequence 1 — field population

`git anchor add <kind> "some text"` has to build a whole document from `<kind>`'s schema plus one CLI argument, and where `"some text"` lands has to fall out of the schema, not be a per-kind special case baked into the CLI.
Two things settle it, in order:

1. **The binding field** is filled by the CLI's own `Binding` (from `--path`/`HEAD`/etc., the existing `capture` pipeline), located by the reflection check above — never by user text.
2. **The positional argument** fills the one remaining field, among those not `Node::Optional`, whose `Node` is `Node::String` (`facet-git-tree` has no separate "text" leaf; `String` is it).
   Exactly one such field is required for the positional form to apply at all; zero or more than one refuses it with an error naming the candidates, and `--json <value>` — a whole `facet_value::Value` literal — is the general escape hatch regardless.
   Applied to `gix-comment`'s document, excluding `binding` and the two `Optional` fields (`parent`, `state`) leaves `body` and `created_at`; only `body` is `Node::String`, so the rule resolves to `body` without the CLI knowing anything about `created_at` specifically.

`created_at: u64` still needs *some* value: `crates/facet-git-tree/src/schema/read.rs`'s `read_struct` requires a tree entry for every field a `Node::Struct` names, `Optional` or not, so a document written with `created_at` simply absent fails on the very next read with `SchemaReadError::MissingField` — whatever `write_named_tree`'s own, looser-sounding doc comment claims about skipping absent fields on write.
The schema gives the CLI no way to know `created_at` specifically wants `now_nanos()`: `facet` does support per-field defaults (`facet_core::Field::default`/`has_default()`, driven by `#[facet(default)]`/`#[facet(default = expr)]`), but that metadata lives on a compiled type's `Shape`, which only `Typed<T>` ever sees — `schema_of` does not carry it into the wire `Schema`/`Node`, whose `Node::Struct` has no per-field default or provenance marker at all.
This is a real upstream gap (`crates/facet-git-tree/src/schema/mod.rs`), not a missed call: closing it means extending `Node::Struct`'s field representation, which the type's own doc comment calls a semver-major `schema.representation` change, and is out of scope for a docs-only decision.

The practical answer that needs no upstream change: the CLI fills every remaining required field with its `Node`'s natural zero value (`0`, `""`, empty `Bytes`/`List`/`Map`, `false`) — computable from the `Node` alone, no default metadata required.
For `created_at` that means a generically-written comment gets `created_at: 0`, a wrong-but-schema-conforming value rather than a crash — which is Consequence 2's tension, not a separate problem: the generic writer can always produce a *conforming* document, never a *meaningful* one.
Zero-filling has no good answer for a required field whose `Node` is a multi-variant `Enum` with no unit variant, or a nested struct that recurses into the same problem; `gix-comment`'s document never hits this (every field besides `binding` is a scalar, a string, or `Optional`), so it does not block this decision, but a future kind that does needs `--json`.

### Consequence 2 — schema conformance is not domain validity

A `Value` that conforms to `<kind>`'s schema is not necessarily one `gix-comment` would have written itself: `state: "banana"`, a `parent` naming no comment that exists, an empty `body`.
`gix-comment`'s domain rules (`open`/`resolved` only, non-blank message) live in `Comments::add`/`resolve`/`reopen`, not in the schema, and `git anchor` never calls them.

Position taken: close what can be closed by putting the vocabulary in the type, and accept the rest as a permanent cost of a schema-only generic writer.

- **`state` is closeable.**
  `crates/facet-git-tree/src/schema/write.rs`'s `write_enum` rejects a tag absent from the schema's declared variant set with `SchemaWriteError::UnknownVariant` — enforced for *every* writer, `Dynamic` included, because the encoder itself does the rejecting, not app-level validation layered on top.
  Giving `gix-comment`'s `State` a `Facet` derive and storing it as `state: Option<State>` directly (done above, in Phase 2) means `"banana"` fails to write at all, through any path — the stronger property this project prefers, achieved here.
  It costs the forward-compatibility leniency `State::from_store`'s doc comment is explicit about wanting today (an old reader must not choke on a state value a newer writer introduced): a third lifecycle state becomes a schema migration event instead of a string an old client shrugs off.
  Taken position: worth it — the vocabulary is fixed and small (open/resolved), and schema evolution has a real migration path (`facet-git-tree`'s `derive`/`Hints`) that free-string leniency does not need but forward-incompatible enum growth would.
- **Non-blank `body` and a `parent` that resolves are not closeable this way.**
  Neither is a shape constraint `Node` can express: `Node::String` has no "non-empty" variant, and `parent` naming another entity is cross-entity referential integrity, entirely outside one document's schema.
  `gix-comment` has to treat every document conforming to its own schema as untrusted external content for these two properties — the same posture `State::from_store` already takes toward an unrecognized string today — so `Comments::get`/`thread`/`hydrate` must handle a blank body or a dangling `parent` as legal-but-degenerate input (an empty rendered comment, a reply that resolves to no parent), never `panic!`/`expect` on the read path.

### What this changes elsewhere in the plan

Phase 3 no longer blocks Phase 2: Phase 2 deletes `gix_anchor::Store`, and Phase 3 is the CLI catching up to that afterward, on its own timeline, over `gix-store` directly rather than through anything `gix-anchor` re-exports.

`crates/git-anchor/Cargo.toml` gains a `gix-store` dependency; `crates/gix-anchor/Cargo.toml` still names neither `gix-store` nor `gix-refstore` (Phase 2's definition of done, unchanged).
That rule is about the *library*, not the applications built on it — `git-anchor`'s binary and `gix-comment` both take on `gix-store` directly, which is the whole point of "a binary is an application."

## Phase 4 — Docs

- `crates/gix-anchor/README.md` — delete the "Store: notes attached to objects" section; state that persistence is the consumer's, over `gix-store`.
- `crates/gix-comment/README.md` — drop "This crate adds no persistence of its own"; it now owns its persistence.
- `crates/git-anchor/README.md`, `crates/git-comment/README.md`, root `README.md` — `git anchor add/list/show/remove <kind>` is generic over the schema registry; `git comment` is the ergonomic front end for the `comment` kind specifically.
- `gix-anchor/src/lib.rs` module doc — the spec-coverage list stays accurate, but the crate is no longer where a consumer's document lives.
- `DEVPLAN-attest.md` — a claim is a document with a `binding: Binding` field, owned by `gix-attest`, over its own `gix-store` layout.
  Its Phase 2 ref-layout question is narrowed by that, and its Phase 3/4 must not plan to reuse `gix_anchor::Store`, which will not exist.

## Definition of done

- `crates/gix-anchor/Cargo.toml` names neither `gix-store` nor `gix-refstore`.
  `crates/git-anchor/Cargo.toml` naming `gix-store` directly is expected, not a violation — see Phase 3.
- `grep -rn 'refs/' crates/gix-anchor/src` finds nothing.
- `crates/gix-anchor/src/store.rs` does not exist.
- `gix-comment`'s published schema records `Binding`'s shape, with a test that locates the binding field in a schema read back from the registry — the anchorable-by-reflection property, asserted rather than assumed.
- `gix-comment`'s `state` is schema-typed (`Option<State>`), not a free string — an unrecognized value fails to write, whatever the writer.
- `cargo test --workspace` passes, doctests included.
- No shim, no re-export of the moved types from `gix-anchor` for convenience.
