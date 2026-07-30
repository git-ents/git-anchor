# git-anchor

Capture a binding and inject it into a document of a kind it was never compiled against.
`git anchor` defines no document type of its own: it reads a kind's published schema out of the registry at run time and writes an entity conforming to it, over `gix-store`'s dynamic (schema-only) write path.

The binary is named `git-anchor`, so git's external-subcommand dispatch makes `git anchor …` work with nothing more than `PATH`.

`inject <kind>` requires `<kind>`'s published schema to embed [`gix_anchor::Binding`](../gix-anchor)'s shape — located by structural comparison against `Binding`'s own schema, not by field name.
That is `git anchor`'s reason to exist: the concrete proof that a kind is anchorable by reflection, not by a per-consumer convention.
The binding field is always filled from a previously captured anchor id, never from user text.

## Demo

`create` captures a binding and writes its identity and hints objects to the object database.
It advances no ref, so it needs no registered kind to run against:

```console
$ git anchor create --path src/lib.rs -L 10,14
a3f1c9e2b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8a9

$ git anchor create --path src/lib.rs -L 10,14   # same coordinates, captured again
a3f1c9e2b4d5f6a7b8c9d0e1f2a3b4c5d6e7f8a9          # identical id — content addressing, not a database lookup
```

`inject` embeds a captured id into an entity of a kind that already has a published schema — `git anchor` never publishes one itself.
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

## Commands

```text
git anchor [--prefix <PREFIX>]                       # bare invocation: every kind with a published schema, anchorable ones marked
git anchor create                                     # capture a binding; writes identity + hints objects, advances no ref; prints the anchor id
    --at <REV>                                        #   the revision the binding names/resolves against; defaults to HEAD; conflicts with --worktree
    --path <PATH>                                     #   anchor a blob path instead of the whole revision, resolved relative to cwd (a git pathspec)
    -L, --lines <START,END[:PATH]>                     #   a 1-based inclusive line range: START,END / START,+COUNT / a single line
                                                        #   number; a trailing :PATH supplies --path in the same token (git log -L's
                                                        #   grammar). Requires a path from either source.
    --worktree                                         #   anchor --path's on-disk content instead of a revision; requires --path,
                                                        #   conflicts with --at
git anchor inject <kind> [<text>] --anchor <ID>       # write an entity of <kind> embedding a previously created binding; <kind> must be anchorable
    --json <VALUE>                                     #   a whole facet_value::Value JSON literal for the document; conflicts with
                                                        #   <text>. The binding field is always the injected --anchor id, overriding
                                                        #   anything this literal sets there.
git anchor list <kind> [--json]                       # every entity of <kind>, name plus value
git anchor show <kind> <name> [--json]                # one entity by its full name, as printed by inject/list
git anchor show <kind> <name>@<rev> [--json]          # project a position binding onto another revision
git anchor show <kind> <name> --worktree [--json]     # project a position binding onto the working tree
git anchor remove <kind> <name>...  (alias: rm)       # delete one or more entities, all checked to exist before any is removed
```

`--prefix <PREFIX>` (default `refs/anchors`) is a global option selecting the store's ref namespace — pass whatever prefix a `gix-store` consumer publishes under.
`<name>` is an entity's full name, `<target-hex>/<id-hex>`, exactly as `inject`/`list` printed it.

`inject`'s one remaining rule, beyond the binding field: among the kind's other required fields, exactly one whose shape is `Node::String` is filled from `<text>`; zero or more than one refuses with an error naming the candidates.
A required field neither the binding nor `<text>` can fill refuses `inject` outright, naming the fields — `--json` is the escape hatch that supplies the whole document explicitly.

## How it works

`git anchor` is a thin CLI over [`gix-store`](https://github.com/git-ents/git-store)'s dynamic (schema-only) read/write path: `inject` fetches `<kind>`'s published `Schema`, locates the field structurally equal to `Binding`'s own schema, and writes a `facet_value::Value` conforming to it — never a compiled Rust type.
`show <name>@<rev>` and `show <name> --worktree` re-derive where a position binding sits elsewhere, exactly as [`gix-anchor`](../gix-anchor)'s `project`/`project_worktree` always did; they operate on the `Binding` extracted from the read entity, not on any document-specific field.
The document embeds the anchor's hints inline, so the retained content stays reachable — no gitlinks, no copies.
`create` and `inject` are deliberately separate verbs: because anchor ids dedup, one `create` can back any number of `inject`s, into any number of kinds, by any number of callers.

## License

Licensed under either of [Apache-2.0](../../LICENSE-APACHE) or [MIT](../../LICENSE-MIT) at your option.
