# Architecture

One rule, from which everything below follows: **exactly one crate per capability.**
Only `facet-git-tree` encodes trees, only `gix-refstore` touches refs, only `gix-store` stores anything, only `gix-attest` touches signatures.
A crate that re-does another's capability is a reimplementation, however well factored it looks locally.

Each crate is *additive over the primitives beneath it* — it adds one concept and delegates everything else downward.

## The ladder

| crate | repo | the one capability | depends on |
| --- | --- | --- | --- |
| `facet-git-tree` | `../git-store` | codec: `Facet` documents ⇄ git trees | `facet`, `gix-object`, `gix-hash` |
| `gix-refstore` | `../git-store` | refs: validated names, read/scan/CAS, committer identity | `gix` |
| `gix-store` | `../git-store` | persistence: entities, kinds, published schemas, commit-forward, history | the two above |
| `gix-anchor` | here | values + projection | *nothing but `gix`* |
| `gix-comment` | here | a comment document | `gix-anchor`, `gix-store` |
| `gix-attest` | here (planned) | claims: signing, revocation, key resolution | `gix-anchor`, `gix-store`, a signing backend |
| `git-query` | elsewhere | moded Datalog over refs; fact-provider seam | `gix-store`, `gix-anchor` |
| `git-forge` | elsewhere | the application | all of the above |

`gix-refstore` is the only crate with ref I/O.
`gix-store` reaches refs solely through `RefStore`/`Committer` trait bounds — it has no `gix_ref` calls of its own, and nothing above it in the ladder writes a commit.

`git-query` deliberately does **not** depend on `gix-attest`: it defines a fact-provider seam that extensions implement, so query stays policy-free and attest stays crypto-only.
`git-forge` is the only crate allowed to wire attest's predicates into query.

## `gix-anchor` is a value-and-projection crate

It defines types and pure functions over them — `Anchor`, `Binding`, `LineRange`, `Oid`; `capture`, `project`, `revalidate`, `diff_trees` — and persists nothing.
No ref, no commit, no storage dependency.

`Binding` is a **vocabulary type**: a well-known, schema-registered `Facet` fragment.
Anchoring a document means embedding that type as a field of your own:

```rust
// gix-comment's document. gix-anchor never sees it.
#[derive(Facet)]
struct Comment {
    body: String,
    binding: Binding,
    parent: Option<String>,
    state: Option<String>,
}
```

`gix-store` then stores a `Comment` exactly as it stores anything else.
It never learns the word "binding"; the binding rides along because it is part of the document's schema.

Three properties fall out, and they are the point:

- **The target is derived, never stored.**
  `Binding::target()` computes it, so a stored target cannot disagree with the binding it came from.
- **A kind is anchorable iff its published schema embeds `Binding`'s shape.**
  A generic consumer — LSP, query engine, forge — reads a schema out of the registry, locates the binding field by reflection, and projects entities of a kind it was never compiled against.
  This is the whole reason `Binding` is a vocabulary type rather than a per-consumer convention, and it requires embedding `Binding` *inline* rather than by opaque tree id: an opaque oid keeps the shape out of the schema and forfeits the property.
- **One entity, one history, one commit.**
  Editing a document and moving its binding are the same atomic write, and "a comment without an anchor" does not typecheck.

## Rejected: the anchor as a separate entity

Storing an anchor as its own entity that *references* the value it binds — by gitlink or by oid — fails on two counts.

**Gitlinks are deliberately opaque to reachability.**
Git does not traverse them for fetch or gc; that is their defining property.
The `anchor.retention` guarantee — anchored content reachable through the document's own tree — would evaporate, and gc could collect the content out from under a live anchor.
`RawTree` embedding is the correct tool precisely because it *is* traversed.

**It makes invalid states representable.**
Two entities means two refs, two histories, and therefore dangling anchors, anchors pointing at a stale version of their value, and non-atomic edit-value/move-binding pairs.

The one case the separate-entity shape genuinely serves — annotating a value you cannot rewrite — is already expressible in the field model, as another ordinary document: `struct Label { subject: RawTree, binding: Binding }`.
Additive, defined by the crate that needs it, no new primitive.

## Rejected: `gix-anchor` owning a note document

`gix-anchor` shipped a `Note` document (`body`, `attachment`, `parent`, `state`) and a `Store` over it.
Three of those fields are documented in that file as opaque passthrough — `gix-anchor` never reads `parent` or `state`; it carried them so `gix-comment` could build reply threads and a lifecycle on them.

That is the same category of error as reimplementing a codec: a primitive holding a downstream consumer's domain shape.
The document belongs to whoever owns the domain.
`DEVPLAN-boundary.md` moves it there.

Two things go with it:

- **Target-first ref grouping** (`<target>/<id>`) is a *naming* choice, made by the consumer through `Kind`'s entity names.
- **Lookup by identity without a target** is an *index*, and `git-query`'s capability.
  What exists today is a linear scan over ref names; the ref layout that makes it cheap is a stopgap, and it now lives in the crate that needs the stopgap.
