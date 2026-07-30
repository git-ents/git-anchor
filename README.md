# git-anchor

Attach arbitrary content to Git objects — a commit, tree, tag, or blob — and optionally to a line range within a blob.
The attachment survives history: an anchor captured against one commit projects onto any later commit, so a comment, review note, or TODO pinned to a span of code follows that code as it moves.

Four crates — the anchor primitive and its CLI, plus a first consumer (comments) and its CLI:

- [`gix-anchor`](crates/gix-anchor) — the library: capture and project
  anchors over a `gix` repository, and the `Binding` vocabulary type; persists
  nothing itself.
- [`git-anchor`](crates/git-anchor) — a git external subcommand
  (`git anchor …`), generic over any registered `gix-store` kind whose
  schema embeds `Binding`.
- [`gix-comment`](crates/gix-comment) — a message pinned to any anchor, built
  on `gix-anchor`'s `Binding` and its own `gix-store`-backed document, whose
  author and timestamp are the storage commit's, plus an optional raw-tree
  attachment, reply threads, and a resolvable open/resolved state.
- [`git-comment`](crates/git-comment) — a git external subcommand
  (`git comment …`), the ergonomic front end for the `comment` kind.

## Demo

```console
$ git comment add --path src/lib.rs -L 10,14 -m "revisit this bound"
dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b

$ git comment list
dd1ebeb2  Ada  open  revisit this bound

$ git comment show dd1ebeb2@main       # where does that span sit on main?
relocated
path: src/lib.rs
lines: 12,16
```

A comment is a real ref and commit — `refs/comments/data/comment/<target-hex>/<id-hex>` — so `git ls-tree`, `git cat-file`, and `git log` inspect it with no application required.
`git anchor --prefix refs/comments add comment "revisit this bound" --path src/lib.rs -L 10,14` writes the identical entity generically, through `git-anchor`, which was never compiled against `gix-comment`'s Rust type — the concrete proof that a kind is anchorable by reflection over its published schema, not by convention.
Projection reports one of four outcomes as the code moves: *current*, *relocated* (new path/lines), *outdated* (an edit touched the span), or *deleted*.
Once the anchored commit is gc'd, it falls back to fuzzy-matching a retained context blob.

See [`docs/specification.adoc`](docs/specification.adoc) for the normative requirements, and each crate's `README` for its API and command reference.
[`ARCHITECTURE.md`](ARCHITECTURE.md) states which capability belongs to which crate — here and in [`git-store`](https://github.com/git-ents/git-store), whose `facet-git-tree`, `gix-refstore`, and `gix-store` do the encoding, ref, and persistence work these crates build on.

## Code of Conduct

Please refer to the in-source [code of conduct](/CONDUCT.md) for all behavioral expectations.

## Contribution Guide

Contributions are welcome.
Please refer to the in-source [contribution guide](/CONTRIBUTING.md).
