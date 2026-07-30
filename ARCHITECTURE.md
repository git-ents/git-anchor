# Architecture

One rule, from which everything below follows: **exactly one crate per capability.**
Only `facet-git-tree` encodes trees, only `gix-refstore` touches refs, only `gix-store` stores anything, only `gix-attest` chains claims.
A crate that re-does another's capability is a reimplementation, however well factored it looks locally.

Each crate is *additive over the primitives beneath it* — it adds one concept and delegates everything else downward.

## The ladder

| crate | repo | the one capability | depends on |
| --- | --- | --- | --- |
| `facet-git-tree` | `../git-store` | codec: `Facet` documents ⇄ git trees | `facet`, `gix-object`, `gix-hash` |
| `gix-refstore` | `../git-store` | refs: validated names, read/scan/CAS, committer identity | `gix` |
| `gix-store` | `../git-store` | persistence: entities, kinds, published schemas, commit-forward, history, and the dynamic (schema-only) read/write path | the two above |
| `gix-anchor` | here | content identity + bind oracles: capture, project, diff | `gix` |
| `git-anchor` | here | CLI: capture a binding and inject it into a document of a registered kind | `gix-anchor`, `gix-store` |
| `gix-attest` | `../git-attest` | claim chains: chaining, revocation, key lifecycle — no signing seam yet | `gix-store` |
| `git-query` | elsewhere | moded Datalog over refs, docs, anchors, claims; `bind/5` composes pin claims over `project` | `gix-store`, `gix-anchor` |
| `git-effect` | elsewhere | rules whose heads are not derivable | `gix-store` |
| `git-forge` | elsewhere | the application layer (comments included) | all of the above |

`gix-refstore` is the only crate with ref I/O.
`gix-store` reaches refs solely through `RefStore`/`Committer` trait bounds — it has no `gix_ref` calls of its own, and nothing above it in the ladder writes a commit.

`git-query` deliberately does **not** depend on `gix-attest`: it defines a fact-provider seam that extensions implement, so query stays policy-free and attest stays envelope-only.
`git-forge` is the only crate allowed to wire attest's predicates into query.

This repo holds exactly two crates: `gix-anchor` (library) and `git-anchor` (CLI).
Comments, and every other application-level document, live in `git-forge`.

## `gix-anchor` is a value-and-projection crate

It defines types and pure functions over them — `Anchor`, `Binding`, `LineRange`, `Oid`; `capture`, `project`, `revalidate`, `diff_trees` — and persists nothing.
No ref, no commit, no storage dependency.

`Binding` is a **vocabulary type**: a well-known, schema-registered `Facet` fragment.
Anchoring a document means embedding that type as a field of your own:

```rust
// A hypothetical consumer's document. gix-anchor never sees it.
#[derive(Facet)]
struct Review {
    body: String,
    binding: Binding,
}
```

`gix-store` then stores a `Review` exactly as it stores anything else.
It never learns the word "binding"; the binding rides along because it is part of the document's schema.

Three properties fall out, and they are the point:

- **The target is derived, never stored.**
  `Binding::target()` computes it, so a stored target cannot disagree with the binding it came from.
- **A kind is anchorable iff its published schema embeds `Binding`'s shape.**
  Locating the field is structural comparison against `Binding`'s own schema, not a per-kind convention.
  The generic machinery this needs — reading a schema out of the registry and writing a value that conforms to it with no compiled Rust type — is `gix-store`'s dynamic (schema-only) write path.
  `git-anchor`'s own job narrows to what belongs to a CLI: capturing a binding and injecting it into a document of a kind it was never compiled against.
  This is the whole reason `Binding` is a vocabulary type rather than a per-consumer convention, and it requires embedding `Binding` *inline* rather than by opaque tree id: an opaque oid keeps the shape out of the schema and forfeits the property.
- **One entity, one history, one commit.**
  Editing a document and moving its binding are the same atomic write, and "a review without an anchor" does not typecheck.

## Identity holds only non-derivable coordinates

An anchor's identity is `(genesis_rev, path, span)` — the commit it was captured against, the path, and the line range (absent for a whole-file anchor).
Nothing versioned or computed belongs there.
`Binding` therefore splits into two sibling subtrees:

- **`identity`** — the coordinates above, and nothing else.
  The anchor id is the content hash of this subtree.
- **`hints`** — retained fingerprint/context material and any structural descriptor (grammar id and version, node kind, qualified name path).
  Additive, versioned, upgradeable, and never identity-bearing.

Fingerprints and descriptors are algorithm- and parameter-versioned.
Put one in identity, and bumping a normalization rule, a shingle width, or a tree-sitter grammar mints a new anchor id for the same span — silently orphaning every pin that referenced the old one.
Keeping them in `hints` means an upgrade changes how a span is *found*, never what it *is*.

## Retain inline, never let it name

Retention and identity are independent properties.
Hints stay inline, as a sibling subtree inside the referring document's own tree, so they stay reachable through that document's ref.
This matters once the genesis commit stops being reachable — after gc, or in a shallow or partial clone: the retained hint is the last surviving evidence, and it is what makes re-anchoring possible offline.

State the rule explicitly, because it is exactly the temptation that produces this bug: **retain it inline, never let it name.**
The previous design conflated "must survive gc" with "must name the thing" — that is how a fingerprint ended up in identity in the first place.
Every future hint field answers to this rule before it ships.

## Anchor identity dedups; anchors have no creator

Content addressing makes the anchor id a pure function of `(genesis_rev, path, span)`.
Two people anchoring the same span at the same genesis compute the same id and share whatever pins reference it — that is intended, not a collision to guard against.
Authorship lives in the document that refers to the anchor — its commit's author — never in the anchor identity, which names a location, not a person.
`git anchor create` reflects this: it is a pure emitter that writes the identity and hints objects and returns the id, advancing no ref.

## `project` is pin-free and threshold-free

`project` returns candidate locations with an oracle and a confidence, and applies no threshold.
Confidence thresholds are policy, and belong to `git-query` as a rule parameter, not to this library.
Pins — human or tool overrides of a bind — are `git-attest` claims; they compose into resolution through `git-query`'s `bind/5` rule, which calls `project` as its derived oracle chain and layers pin claims over it.
`gix-anchor` and `git-attest` are siblings: neither depends on the other, so `project` structurally cannot see a pin, and nothing in this crate should read as though it could.
The user-facing resolution entry point is query's `bind/5`; `project` is the oracle chain it calls.

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

`gix-anchor` once shipped a `Note` document (`body`, `attachment`, `parent`, `state`) and a `Store` over it.
Three of those fields were opaque passthrough — `gix-anchor` never read `parent` or `state`; it carried them only so a downstream comment consumer could build reply threads and a lifecycle on them.

That is the same category of error as reimplementing a codec: a primitive holding a downstream consumer's domain shape.
The document belongs to whoever owns the domain — comments live in `git-forge` now, not here.

Two things go with it:

- **Target-first ref grouping** (`<target>/<id>`) is a *naming* choice, made by the consumer through `Kind`'s entity names.
- **Lookup by identity without a target** is an *index*, and `git-query`'s capability.
  A linear scan over ref names is a stopgap; the crate that needs the stopgap owns it.

## Deferred, deliberately

Cryptographic signing, operation logs, and action-cache keys are out of scope for now.
Authority is simply a ref transition in a repository you trust — there is no `Signer` seam in `gix-store`.
The codec writes a typed tree with every field present as a sentinel, so an empty `hints` subtree or an empty signature field costs nothing to fill in later: deferring these is not a migration debt.
