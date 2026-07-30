# gix-anchor

Attach arbitrary content to Git objects — and, for a blob, to a line range within it — such that the attachment *follows the content* as history moves.

An **anchor** is a durable pointer into source: a blob, an optional 1-based inclusive line range, and the commit it was captured against.
Anchors resolve and *project* independently of any consumer — a comment is merely the first client; reviews, TODO trackers, and blame overlays reuse the same mechanism.
The library is `gix`-native and oid-in/oid-out: it performs its own object reads and ref writes over a [`gix::Repository`], with no materialized worktree required for diffs.

See [`docs/specification.adoc`](../../docs/specification.adoc) for the normative requirements this crate implements (`anchor.definition`, `anchor.immutable`, `anchor.retention`, `anchor.projection`, `anchor.fuzzy-fallback`, `anchor.working-tree`, `anchor.tree-pair-diff`).

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

## Store: notes attached to objects

`Store` persists an anchor together with an arbitrary content *body* under `refs/anchors/data/notes/<target>/<binding-id>`, with `git notes`-style semantics: one editable note per anchored target, and every re-attach records a new version on the same ref, so history comes for free.

```rust
use gix_anchor::{capture, Binding, Store};

let repo = gix::discover(".")?;
let store = Store::open(&repo);

// Attach a note to a line range …
let anchor = capture(&repo, "HEAD", "src/lib.rs", None)?;
let id = store.attach(&Binding::Position(anchor), b"needs a doc comment", None)?;

// … read it back, with the anchor and body recovered.
let note = store.get(id)?.expect("just attached");
assert_eq!(note.body, b"needs a doc comment");

// List, or filter to one target object; remove when done.
for note in store.list(None)? {
    println!("{} -> {}", note.id, String::from_utf8_lossy(&note.body));
}
store.remove(id)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The stored note embeds the anchor's serialized tree by object id, so the anchored blob and its context stay reachable from the note's own tree — the `anchor.retention` requirement — never via a gitlink.

## What can be a target

A [`Binding`] names the object a note is attached to:

- `Position(Anchor)` — a blob, optionally a line range within it.
- `Commit`, `Tree` — a whole object.
- `Delta`, `Hybrid` — a change between two trees, or a commit paired with a tree.

`Binding::target()` returns the primary object id, which is also the ref-path grouping key in the store.

## Tree-pair diff

[`diff_trees`] is a standalone structural diff over any [`gix_object::Find`] source: it walks two trees in lockstep, prunes any subtree with an equal object id on both sides, and reports one [`TreeChange`] per differing entry.
Either side may be the empty tree.
It requires only object reads.

## License

Licensed under either of [Apache-2.0](../../LICENSE-APACHE) or [MIT](../../LICENSE-MIT) at your option.
