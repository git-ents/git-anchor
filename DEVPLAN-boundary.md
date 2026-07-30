# Crate boundaries — make `gix-anchor` storage-free

**Status: done (2026-07-30).**
Phases 1–4 all shipped; each phase below carries a *Shipped* note where the outcome differs from the plan.
One divergence spans phases: Phase 3 landed before Phase 2 (see Phase 2's note), so the two are numbered by design intent, not by commit order.

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

**Shipped, 2026-07-30 (`f18e04e`).**
As planned: `Note.binding` became an inline `Binding` field in `gix-anchor/src/store.rs`, and binding-keyed identity (`attach`) stayed available as a consumer convention pending Phase 2's decision — which dropped it outright rather than moving it, since genesis-keyed `gix-comment` never needed it.

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

**Shipped, 2026-07-30 (`2bb8f2f`), after Phase 3, not before.**
This plan's own numbering assumed Phase 2 lands first, since it is the one that deletes `gix_anchor::Store`.
In fact `4014fff` (Phase 3, making `git anchor` generic) shipped first: `git-anchor`'s `main.rs` was still calling `gix_anchor::Store` directly, so deleting `Store` before the CLI stopped depending on it would have left the workspace uncompilable for the length of Phase 2's own commit.
Making the CLI generic over `gix-store` first, then deleting `Store` once nothing referenced it, kept every commit on `main` buildable.
The phases are numbered by design dependency (Phase 3's equivalence claim needs Phase 2's document shape to be true), not by ship order.

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

**Shipped, 2026-07-30 (`fcc7765`, `4014fff`), before Phase 2, not after — see Phase 2's note.**
The equivalence this phase opens with was, as written below, a goal rather than something Phase 3 alone delivered: it needed three document-shape changes this plan had not enumerated up front.
`fcc7765` closed the two upstream gaps named below (a field-level `has_default` marker on the wire `Schema`, and a struct write that omits a schema-required field now erroring instead of silently succeeding).
On top of that, `gix-comment`'s document needed `body: String` (so the positional argument has a `Node::String` field to land on — Phase 2's design already had this), `state: Option<State>` (Phase 2's design already had this too), and `created_at`'s `#[facet(default = now_nanos())]` (the concrete field-default marker Consequence 1 below named as unscheduled upstream work, now scheduled and landed alongside it).
With all three in place, `add_against_the_real_comment_kind_matches_gix_comment_add` (`crates/git-anchor/tests/cli.rs`) proves `git anchor add comment "some text"` and `git comment add "some text"` write entities `gix-comment`'s own typed reader cannot tell apart.

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

`created_at: u64` still needs *some* value: `crates/facet-git-tree/src/schema/read.rs`'s `read_struct` requires a tree entry for every field a `Node::Struct` names, `Optional` or not, so a document written with `created_at` simply absent fails on the very next read with `SchemaReadError::MissingField` — whatever `write_named_tree`'s own, looser-sounding doc comment claims about skipping absent fields on write, itself a second upstream inconsistency named below.
The schema gives the CLI no way to know `created_at` specifically wants `now_nanos()`: `facet` does support per-field defaults (`facet_core::Field::default`/`has_default()`, driven by `#[facet(default)]`/`#[facet(default = expr)]`), but that metadata lives on a compiled type's `Shape`, which only `Typed<T>` ever sees — `schema_of` does not carry it into the wire `Schema`/`Node`, whose `Node::Struct` has no per-field default or provenance marker at all.
This is a real upstream gap (`crates/facet-git-tree/src/schema/mod.rs`), not a missed call: closing it means extending `Node::Struct`'s field representation, which the type's own doc comment calls a semver-major `schema.representation` change — named as upstream work this phase depends on, below, not worked around here.

`created_at` is an ordering key, not a degenerate value the CLI can shrug off: `crates/gix-anchor/src/store.rs` documents it as a nanosecond timestamp, a finer-grained tiebreak than a commit's one-second author time, and `crates/gix-comment/src/comment.rs` sorts replies on it.
A comment written with `created_at: 0` would sort before every other comment in its thread, permanently, with nothing to signal that anything went wrong — silent data corruption, not a wrong-but-tolerable value.
The generic writer therefore never invents a value for a required field it cannot populate: after the binding field and the positional argument are filled (the two rules above, unchanged), `git anchor add <kind> …` refuses if any remaining field is required — not `Node::Optional` — and still unfilled, with an error naming those fields and pointing the caller at `--json <value>` to supply the whole document explicitly.

Applied to `gix-comment`'s document, this refusal is not hypothetical: `created_at` is required and the schema gives the CLI no way to fill it, so `git anchor add comment "some text"` does not work today — it refuses, naming `created_at`.
The equivalence Phase 3 opens with — `git anchor add comment "some text"` is `git comment add "some text"` by another name — is therefore a goal this phase moves toward, not something Phase 3 delivers the moment it ships: it holds only once the upstream item below is closed.
A plan that claimed the command works today, when it would refuse, would be worse than one that names the dependency.

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

### Upstream work this phase depends on

Two gaps in `../git-store` are load-bearing for this phase, not incidental: the standing rule this project follows (`DEVPLAN-storage.md`'s Phase 0 "Shipped" note) is that a gap in a `../git-store` crate gets fixed upstream, not worked around downstream.

- **Field default/provenance metadata in `Schema`.**
  `crates/facet-git-tree/src/schema/mod.rs`'s `Node::Struct(BTreeMap<String, Node>)` carries no per-field marker distinguishing a field the writer must supply from one it may leave for something else to fill; closing Consequence 1's gap needs that added to `Node::Struct`'s field representation, which the type's own doc comment already calls a semver-major `schema.representation` change.
  Pick a field-level default-presence marker over carrying the default value itself: `created_at`'s wanted default (`now_nanos()`) is computed at write time, not a fixed constant, so baking a snapshot into the schema reproduces the exact silent-corruption failure Consequence 1 rejects zero-filling for, just relocated upstream.
  A marker also keeps `Schema`/`Node` a pure shape descriptor — no other `Node` variant carries a value, and a default-value variant would be the sole exception.
  Unscheduled: a named dependency of Phase 3's `add` working for `gix-comment`'s document, not a blocker on this commit.
- **`read_struct`/`write_named_tree` disagree on an absent `Optional` field.**
  Confirmed by reading both: `read_struct` (`crates/facet-git-tree/src/schema/read.rs`) requires a tree entry for every field a `Node::Struct` names, `Optional` included — a field's `None` is the entry pointing at the presence-marker tree, never the entry's absence — while `write_named_tree` (`crates/facet-git-tree/src/schema/write.rs`) silently skips any field absent from the input object.
  `write_named_tree`'s doc comment is the stale side: it claims to match "the read path's leniency," but `read_struct`'s own doc comment says strictness was added specifically to make it usable as a conformance check, which the two functions no longer agree on.
  A `Value` built by a caller who omits an `Optional` field, rather than setting it `null`, writes today without error and then fails to read back with `SchemaReadError::MissingField` — a document that round-trips one way only.
  Named as a second upstream item; not fixed here.

Whether `created_at` should exist in the document at all: keep it, and close the metadata gap above, rather than drop it and fall back to a commit's one-second author-time resolution with the oid as tiebreak.
`gix-comment`'s own reply sort (`crates/gix-comment/src/comment.rs`) already leans on nanosecond resolution specifically because two replies landing in the same second are not a corner case worth losing precision over.

### What this changes elsewhere in the plan

Phase 3 no longer blocks Phase 2: Phase 2 deletes `gix_anchor::Store`, and Phase 3 is the CLI catching up to that afterward, on its own timeline, over `gix-store` directly rather than through anything `gix-anchor` re-exports.

**This is the one part of the plan that shipped backwards.**
Phase 3 landed first, not "afterward": with `git-anchor` still calling `gix_anchor::Store` up to the moment Phase 3 removed that call, deleting `Store` first (as this paragraph assumes) would have broken the build for the commit in between.
See Phase 2's shipped note.

`crates/git-anchor/Cargo.toml` gains a `gix-store` dependency; `crates/gix-anchor/Cargo.toml` still names neither `gix-store` nor `gix-refstore` (Phase 2's definition of done, unchanged).
That rule is about the *library*, not the applications built on it — `git-anchor`'s binary and `gix-comment` both take on `gix-store` directly, which is the whole point of "a binary is an application."

## Phase 4 — Docs

**Shipped, 2026-07-30.**
`crates/gix-anchor/README.md`, `crates/gix-comment/README.md`, `crates/git-anchor/README.md`, `crates/git-comment/README.md`, and root `README.md` all updated per the checklist below; `gix-anchor/src/lib.rs`'s doctest needed no change — it already stood a local `Comment`-shaped struct in for a consumer's document rather than importing `gix_anchor::Store`.
`DEVPLAN-attest.md` already carried the required note from an earlier pass; nothing to add.

- `crates/gix-anchor/README.md` — delete the "Store: notes attached to objects" section; state that persistence is the consumer's, over `gix-store`.
- `crates/gix-comment/README.md` — drop "This crate adds no persistence of its own"; it now owns its persistence.
- `crates/git-anchor/README.md`, `crates/git-comment/README.md`, root `README.md` — `git anchor add/list/show/remove <kind>` is generic over the schema registry; `git comment` is the ergonomic front end for the `comment` kind specifically.
- `gix-anchor/src/lib.rs` module doc — the spec-coverage list stays accurate, but the crate is no longer where a consumer's document lives.
- `DEVPLAN-attest.md` — a claim is a document with a `binding: Binding` field, owned by `gix-attest`, over its own `gix-store` layout.
  Its Phase 2 ref-layout question is narrowed by that, and its Phase 3/4 must not plan to reuse `gix_anchor::Store`, which will not exist.

## Definition of done

**All items hold, verified 2026-07-30.**

- `crates/gix-anchor/Cargo.toml` names neither `gix-store` nor `gix-refstore`.
  `crates/git-anchor/Cargo.toml` naming `gix-store` directly is expected, not a violation — see Phase 3.
- `grep -rn 'refs/' crates/gix-anchor/src` finds nothing.
- `crates/gix-anchor/src/store.rs` does not exist.
- `gix-comment`'s published schema records `Binding`'s shape, with a test that locates the binding field in a schema read back from the registry — the anchorable-by-reflection property, asserted rather than assumed.
- `gix-comment`'s `state` is schema-typed (`Option<State>`), not a free string — an unrecognized value fails to write, whatever the writer.
- `cargo test --workspace` passes, doctests included.
- No shim, no re-export of the moved types from `gix-anchor` for convenience.
