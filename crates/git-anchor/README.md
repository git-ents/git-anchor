# git-anchor

`git-anchor` is one crate with two faces: a **library** that captures and projects anchors over a `gix` repository — the `Binding` vocabulary type and the three oracles that map an anchor onto another revision — and, behind its default-on `cli` feature, the **`git anchor`** external subcommand that captures a binding and injects it into a document of a kind it was never compiled against.

See [`docs/specification.adoc`](../../docs/specification.adoc) for the normative requirements the library implements (`anchor.definition`, `anchor.identity`, `anchor.immutable`, `anchor.retention`, `anchor.projection`, `anchor.fuzzy-fallback`, `anchor.working-tree`, `anchor.tree-pair-diff`).

## The library

An **anchor** is a durable pointer into source: a genesis commit, a repository-relative path, and a byte span over the blob's stored (post-clean-filter) bytes — nothing else.
That triple is an anchor's *identity*; the anchor id is the content hash of it, computed through the identity normal form.
A caller may supply a 1-based inclusive line range at capture time — `git anchor create -L`'s own input — but it is canonicalized to a byte span immediately and never itself appears in the identity.
Everything else the library retains — content fingerprints, structural descriptors — is a *hint*: additive, versioned, upgradeable, and never part of the id.
Anchors resolve independently of any consumer, through three named oracles — reviews, TODO trackers, and blame overlays all reuse the same mechanism.
The library is `gix`-native and oid-in/oid-out: it performs its own object reads over a [`gix::Repository`], with no materialized worktree required for diffs.
It persists nothing itself — no ref, no commit; persistence belongs to the consumer, over `git-store`.

### Capture and the oracles

```rust
use git_anchor::{capture, diff_trace, LineRange};

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
Combining all three into one ranked, thresholded answer is `git-query`'s `bind/5`, not the library's job: these oracles are library-internal building blocks, not a user-facing resolution API.

### `Binding`: what can be anchored to

A `Binding` names the object an anchor is attached to:

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

### Tree-pair diff

`diff_trees` is a standalone structural diff over any [`gix_object::Find`] source: it walks two trees in lockstep, prunes any subtree with an equal object id on both sides, and reports one `TreeChange` per differing entry.
Either side may be the empty tree.
It requires only object reads.

## The `git anchor` CLI

Capture a binding and inject it into a document of a kind it was never compiled against.
`git anchor` defines no document type of its own: it reads a kind's published schema out of the registry at run time and writes an entity conforming to it, over `git-store`'s dynamic (schema-only) write path.

The binary is named `git-anchor`, so git's external-subcommand dispatch makes `git anchor …` work with nothing more than `PATH`.

`inject <kind>` requires `<kind>`'s published schema to embed `git_anchor::Binding`'s shape — located by structural comparison against `Binding`'s own schema, not by field name.
That is `git anchor`'s reason to exist: the concrete proof that a kind is anchorable by reflection, not by a per-consumer convention.
The binding field is always filled from a previously captured handle, never from user text.

`create` prints a **capture handle**, not an anchor id: the oid of the whole serialized binding (identity and hints together).
It exists only to carry a capture from `create` to `inject`.
The **anchor id** — the content hash of the binding's `identity` subtree alone, invariant under any hint change — is what a pin cites and what dedups; read it off a handle with `git anchor id`.

### Demo

`create` captures a binding and writes its identity and hints objects to the object database.
It advances no ref, so it needs no registered kind to run against:

```console
$ git anchor create --path src/lib.rs -L 10,14
a3f1c9e2b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8a9

$ git anchor create --path src/lib.rs -L 10,14   # same coordinates and repository state, captured again
a3f1c9e2b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8a9          # identical handle — content addressing, not a database lookup

$ git anchor id a3f1c9e2b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8a9   # the anchor id: identity's oid alone
7c2b1a4f5e6d7c8b9a0f1e2d3c4b5a6f7e8d9c0b
```

`inject` embeds a captured handle into an entity of a kind that already has a published schema — `git anchor` never publishes one itself.
The demo below assumes some other tool has already published an anchorable kind called `note` under `refs/forge` — this repo ships none itself:

```console
$ git anchor --prefix refs/forge                                          # every kind with a published schema, anchorable ones marked
note  (anchorable)

$ git anchor --prefix refs/forge inject note "revisit this bound" --anchor a3f1c9e2b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8a9
7a28df3c975fa62270a452251c4e0b24d685c4ba/9f2c7a1b4e5d6c3a2b1f0e9d8c7b6a5f4e3d2c1b

$ git anchor --prefix refs/forge list note
7a28df3c975fa62270a452251c4e0b24d685c4ba/9f2c7a1b4e5d6c3a2b1f0e9d8c7b6a5f4e3d2c1b  {"body":"revisit this bound", …}

$ git anchor --prefix refs/forge remove note 7a28df3c975fa62270a452251c4e0b24d685c4ba/9f2c7a1b4e5d6c3a2b1f0e9d8c7b6a5f4e3d2c1b
```

### Commands

```text
git anchor [--prefix <PREFIX>]                       # bare invocation: every kind with a published schema, anchorable ones marked
git anchor create                                     # capture a binding; writes identity + hints objects, advances no ref; prints the capture handle
    --at <REV>                                        #   the revision the binding names/resolves against; defaults to HEAD; conflicts with --worktree
    --path <PATH>                                     #   anchor a blob path instead of the whole revision, resolved relative to cwd (a git pathspec)
    -L, --lines <START,END[:PATH]>                     #   a 1-based inclusive line range: START,END / START,+COUNT / a single line
                                                        #   number; a trailing :PATH supplies --path in the same token (git log -L's
                                                        #   grammar). Requires a path from either source.
    --worktree                                         #   anchor --path's on-disk content instead of a revision; requires --path,
                                                        #   conflicts with --at
git anchor id <HANDLE>                                # print the anchor id: the identity subtree's oid, invariant under any hint change
git anchor inject <kind> [<text>] --anchor <HANDLE>   # write an entity of <kind> embedding a previously created binding; <kind> must be anchorable
    --json <VALUE>                                     #   a whole facet_value::Value JSON literal for the document; conflicts with
                                                        #   <text>. The binding field is always the injected --anchor handle, overriding
                                                        #   anything this literal sets there.
git anchor list <kind> [--json]                       # every entity of <kind>, name plus value
git anchor show <kind> <name> [--json]                # one entity by its full name, as printed by inject/list — exactly as stored
git anchor remove <kind> <name>...  (alias: rm)       # delete one or more entities, all checked to exist before any is removed
```

`--prefix <PREFIX>` (default `refs/anchors`) is a global option selecting the store's data ref namespace — entities land directly under it (e.g. `refs/forge/<kind>/…` for `git forge`'s store).
Schemas always come from `refs/schema`, the shared `git-store` default, so a kind published by any `git-store` consumer is injectable here and vice versa.
`inject` also publishes `refs/anchors/<anchor-id>` — the registry `git query`'s `bind/5` resolves through — for `Binding::Position` bindings.
`<name>` is an entity's full name, `<anchor-id-hex>/<id-hex>`, exactly as `inject`/`list` printed it.

`inject`'s one remaining rule, beyond the binding field: among the kind's other required fields, exactly one whose shape is `Node::String` is filled from `<text>`; zero or more than one refuses with an error naming the candidates.
A required field neither the binding nor `<text>` can fill refuses `inject` outright, naming the fields — `--json` is the escape hatch that supplies the whole document explicitly.

### How it works

`git anchor` is a thin CLI over [`git-store`](https://github.com/git-ents/git-store)'s dynamic (schema-only) read/write path: `inject` fetches `<kind>`'s published `Schema`, locates the field structurally equal to `Binding`'s own schema, and writes a `facet_value::Value` conforming to it — never a compiled Rust type.
`show` prints an entity exactly as stored and resolves nothing: mapping a position binding onto another revision is the library's oracle chain (`diff_trace`/`fingerprint_oracle`/`op_log`), which is library-internal — no command here calls it.
Resolution is `git-query`'s `bind/5`.
The document embeds the anchor's hints inline, so they stay reachable — no gitlinks, no copies.
`create` and `inject` are deliberately separate verbs: because a capture handle is content-addressed, one `create` can back any number of `inject`s, into any number of kinds, by any number of callers.

## License

Licensed under either of [Apache-2.0](../../LICENSE-APACHE) or [MIT](../../LICENSE-MIT) at your option.
