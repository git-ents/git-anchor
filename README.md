# git-anchor

Attach arbitrary content to Git objects — a commit, tree, tag, or blob — and optionally to a line range within a blob.
The attachment survives history: an anchor captured against one commit projects onto any later commit, so a comment, review note, or TODO pinned to a span of code follows that code as it moves.

Two crates:

- [`gix-anchor`](crates/gix-anchor) — the library: capture, store, and project
  anchors over a `gix` repository.
- [`git-anchor`](crates/git-anchor) — a git external subcommand (`git anchor …`).

## Demo

```console
$ git anchor add --path src/lib.rs -L 10,14 -m "revisit this bound"
dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b

$ git anchor list                     # <id>  <target>  <body>
dd1ebeb2  7a28df3c  revisit this bound

$ git anchor show dd1ebeb2@main       # where does that span sit on main?
relocated
path: src/lib.rs
lines: 12,16
```

A note is a real ref and commit — `refs/anchors/<target>/<id>` — so `git ls-tree`, `git cat-file`, and `git log` inspect it with no application required.
Projection reports one of four outcomes as the code moves: *current*, *relocated* (new path/lines), *outdated* (an edit touched the span), or *deleted*.
Once the anchored commit is gc'd, it falls back to fuzzy-matching a retained context blob.

See [`docs/specification.adoc`](docs/specification.adoc) for the normative requirements, and each crate's `README` for its API and command reference.

## Code of Conduct

Please refer to the in-source [code of conduct](/CONDUCT.md) for all behavioral expectations.

## Contribution Guide

Contributions are welcome.
Please refer to the in-source [contribution guide](/CONTRIBUTING.md).
