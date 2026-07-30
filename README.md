# git-anchor

Attach arbitrary content to Git objects — a commit, tree, tag, or blob — and optionally to a line range within a blob.
The attachment survives history: an anchor captured against one commit projects onto any later commit, so a review note or TODO pinned to a span of code follows that code as it moves.

Two crates:

- [`gix-anchor`](crates/gix-anchor) — the library: capture and project anchors over a `gix` repository, and the `Binding` vocabulary type; persists nothing itself.
- [`git-anchor`](crates/git-anchor) — a git external subcommand (`git anchor …`) that captures a binding and injects it into a document of any `gix-store` kind whose published schema embeds `Binding`'s shape.

## Demo

```console
$ git anchor create --path src/lib.rs -L 10,14
a3f1c9e2b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8a9

$ git anchor create --path src/lib.rs -L 10,14   # captured again, same coordinates
a3f1c9e2b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8a9          # identical id: anchors dedup, and have no creator
```

`create` is a pure emitter: it writes the identity and hints objects and prints the anchor id, advancing no ref.
Injecting that id into a document — `git anchor inject <kind> … --anchor <id>` — is the second half of the CLI; see [`git-anchor`'s README](crates/git-anchor) for the full command reference and a worked example against a registered kind.

See [`docs/specification.adoc`](docs/specification.adoc) for the normative requirements, and each crate's `README` for its API and command reference.
[`ARCHITECTURE.md`](ARCHITECTURE.md) states which capability belongs to which crate — here and in [`git-store`](https://github.com/git-ents/git-store), whose `facet-git-tree`, `gix-refstore`, and `gix-store` do the encoding, ref, and persistence work these crates build on.

## Code of Conduct

Please refer to the in-source [code of conduct](/CONDUCT.md) for all behavioral expectations.

## Contribution Guide

Contributions are welcome.
Please refer to the in-source [contribution guide](/CONTRIBUTING.md).
