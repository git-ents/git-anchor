# gix-anchor

Attach arbitrary content to Git objects — and, for a blob, to a line range within it — such that the attachment *follows the content* as history moves.

An **anchor** is a durable pointer into source: a genesis commit, a repository-relative path, and an optional 1-based inclusive line range — nothing else.
That triple is an anchor's *identity*; the anchor id is the content hash of it.
Everything else this crate retains — the anchored blob's own copy, a context window, any structural descriptor — is a *hint*: additive, versioned, upgradeable, and never part of the id.
Anchors resolve and *project* independently of any consumer — reviews, TODO trackers, and blame overlays all reuse the same mechanism.
The library is `gix`-native and oid-in/oid-out: it performs its own object reads over a [`gix::Repository`], with no materialized worktree required for diffs.
It persists nothing itself — no ref, no commit; persistence belongs to the consumer, over `gix-store`.

See [`docs/specification.adoc`](../../docs/specification.adoc) for the normative requirements this crate implements (`anchor.definition`, `anchor.identity`, `anchor.immutable`, `anchor.retention`, `anchor.projection`, `anchor.fuzzy-fallback`, `anchor.working-tree`, `anchor.tree-pair-diff`).

## Capture and project

```rust
use gix_anchor::{capture, project, LineRange, Projection};

let repo = gix::discover(".")?;

// Capture lines 10–14 of a file as of some commit.
let anchor = capture(&repo, "HEAD", "src/lib.rs", Some(LineRange { start: 10, end: 14 }))?;

// Project it onto another commit: is that span still there, did it move,
// was it edited, or is the file gone?
match project(&repo, &anchor, "main")? {
    Projection::Current => println!("unchanged"),
    Projection::Relocated { path, lines } => println!("moved to {path} {lines:?}"),
    Projection::Outdated { path } => println!("edited in {path}"),
    Projection::Deleted => println!("file removed"),
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Projection follows renames and works between any two commits — forwards, backwards, or across unrelated history — as long as the anchored commit still exists.
Once that commit is garbage-collected, projection degrades to fuzzily matching a retained *context* blob against the target, recovering the same four outcomes approximately rather than exactly.

## `Binding`: what can be anchored to

A [`Binding`] names the object an anchor is attached to:

- `Position(Anchor)` — a blob, optionally a line range within it.
- `Commit`, `Tree` — a whole object.
- `Delta`, `Hybrid` — a change between two trees, or a commit paired with a tree.

`Binding::anchor_id()` returns the identity subtree's oid, which a consumer typically uses as its own ref-path grouping key.
`Binding` is a **vocabulary type**: embed it as an inline field of your own `Facet` document —

```rust
#[derive(Facet)]
struct Review { body: String, binding: Binding }
```

— and a generic consumer (`git anchor`, an LSP, a query engine) can discover that the kind is anchorable by structural comparison against `Binding`'s own schema, with no per-kind convention.
Internally `Binding` splits into two sibling subtrees: `identity`, the non-derivable coordinates the anchor id hashes, and `hints`, everything retained for approximate re-anchoring.
Two captures of the same coordinates produce the same id and share whatever refers to it; nothing about *who* captured it is part of the anchor.
See [`ARCHITECTURE.md`](../../ARCHITECTURE.md) for the identity/hints split, why the field is embedded inline rather than referenced by tree id, and why `project` here never sees a pin.

## Tree-pair diff

[`diff_trees`] is a standalone structural diff over any [`gix_object::Find`] source: it walks two trees in lockstep, prunes any subtree with an equal object id on both sides, and reports one [`TreeChange`] per differing entry.
Either side may be the empty tree.
It requires only object reads.

## License

Licensed under either of [Apache-2.0](../../LICENSE-APACHE) or [MIT](../../LICENSE-MIT) at your option.
