# gix-anchor

Attach arbitrary content to Git objects — and, for a blob, to a line range within it — such that the attachment *follows the content* as history moves.

An **anchor** is a durable pointer into source: a genesis commit, a repository-relative path, and a byte span over the blob's stored (post-clean-filter) bytes — nothing else.
That triple is an anchor's *identity*; the anchor id is the content hash of it, computed through the identity normal form.
A caller may supply a 1-based inclusive line range at capture time — `git anchor create -L`'s own input — but it is canonicalized to a byte span immediately and never itself appears in the identity.
Everything else this crate retains — content fingerprints, structural descriptors — is a *hint*: additive, versioned, upgradeable, and never part of the id.
Anchors resolve independently of any consumer, through three named oracles — reviews, TODO trackers, and blame overlays all reuse the same mechanism.
The library is `gix`-native and oid-in/oid-out: it performs its own object reads over a [`gix::Repository`], with no materialized worktree required for diffs.
It persists nothing itself — no ref, no commit; persistence belongs to the consumer, over `gix-store`.

See [`docs/specification.adoc`](../../docs/specification.adoc) for the normative requirements this crate implements (`anchor.definition`, `anchor.identity`, `anchor.immutable`, `anchor.retention`, `anchor.projection`, `anchor.fuzzy-fallback`, `anchor.working-tree`, `anchor.tree-pair-diff`).

## Capture and the oracles

```rust
use gix_anchor::{capture, diff_trace, LineRange};

let repo = gix::discover(".")?;

// Capture lines 10–14 of a file as of some commit; the range is
// canonicalized to a byte span before it ever reaches `identity`.
let anchor = capture(&repo, "HEAD", "src/lib.rs", Some(LineRange { start: 10, end: 14 }))?;

// Map it onto another commit by exact history tracing: zero or more
// candidates, each carrying which oracle produced it and that oracle's
// own confidence — never a threshold anchor applies itself.
for candidate in diff_trace(&repo, &anchor, "main")? {
    println!("{:?} at {} (confidence {})", candidate.span, candidate.path, candidate.confidence);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`diff_trace` follows renames and works between any two commits — forwards, backwards, or across unrelated history — as long as the anchor's own commit still exists; it reports no candidates, never an error, once that commit is gone.
`fingerprint_oracle` fuzzy-matches a retained (or freshly recomputed) content fingerprint against the target instead, for exactly that case.
`op_log` is a seam — a trait a future operation-log adapter implements — that yields nothing when the caller supplies none.
Combining all three into one ranked, thresholded answer is `git-query`'s `bind/5`, not this crate's job: these oracles are library-internal building blocks, not a user-facing resolution API.

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
Internally `Binding` splits into two sibling subtrees: `identity`, the non-derivable coordinates the anchor id hashes, and `hints` — fingerprints and structural descriptors — retained for approximate re-anchoring.
Two captures of the same coordinates produce the same id and share whatever refers to it; nothing about *who* captured it is part of the anchor.
See [`ARCHITECTURE.md`](../../ARCHITECTURE.md) for the identity/hints split, why the field is embedded inline rather than referenced by tree id, and why the oracle chain here never sees a pin.

## Tree-pair diff

[`diff_trees`] is a standalone structural diff over any [`gix_object::Find`] source: it walks two trees in lockstep, prunes any subtree with an equal object id on both sides, and reports one [`TreeChange`] per differing entry.
Either side may be the empty tree.
It requires only object reads.

## License

Licensed under either of [Apache-2.0](../../LICENSE-APACHE) or [MIT](../../LICENSE-MIT) at your option.
