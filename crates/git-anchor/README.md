# git-anchor

Write, read, and remove entities of any `gix-store` kind, generic over its published schema.
`git anchor` defines no document type of its own: it reads a kind's schema out of the registry at run time and writes an entity of it, without ever having been compiled against that kind's Rust type.

The binary is named `git-anchor`, so git's external-subcommand dispatch makes `git anchor …` work with nothing more than `PATH`.

`add <kind>` requires `<kind>`'s published schema to embed [`gix_anchor::Binding`](../gix-anchor)'s shape — located by structural comparison against `Binding`'s own schema, not by field name.
That is `git anchor`'s reason to exist: the concrete proof that a kind is anchorable by reflection, not by a per-consumer convention.
The binding field is always filled from the CLI's own capture pipeline (`--at`/`--path`/`-L`/`--worktree`), never from user text.

## Demo

`git anchor add` requires a kind that already has a published schema — `git anchor` never publishes one itself.
`git comment` publishes the `comment` kind as a side effect of its first write, so the same store is reachable from either tool by pointing `--prefix` at `refs/comments`:

```console
$ git comment add HEAD -m "reviewed, ship it"       # publishes the `comment` kind's schema as a side effect
dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b

$ git anchor --prefix refs/comments                 # every kind with a published schema, anchorable ones marked
comment  (anchorable)

$ git anchor --prefix refs/comments add comment "ship it, second take"
9f2c7a1b4e5d6c3a2b1f0e9d8c7b6a5f4e3d2c1b

$ git anchor --prefix refs/comments list comment
7a28df3c975fa62270a452251c4e0b24d685c4ba/9f2c7a1b4e5d6c3a2b1f0e9d8c7b6a5f4e3d2c1b  {"body":"ship it, second take", …}

$ git anchor --prefix refs/comments remove comment 7a28df3c975fa62270a452251c4e0b24d685c4ba/9f2c7a1b4e5d6c3a2b1f0e9d8c7b6a5f4e3d2c1b
```

`git anchor add comment "some text"` is `git comment add "some text"` by another name: same write, same ref namespace, same schema — `gix-comment`'s own typed reader recovers the exact entity `git anchor` wrote.

## Commands

```text
git anchor [--prefix <PREFIX>]                    # bare invocation: every kind with a published schema, anchorable ones marked
git anchor add <kind> [<text>]                    # write an entity of <kind>; <kind> must be anchorable
    --at <REV>                                    #   the revision the binding names/resolves against; defaults to HEAD; conflicts with --worktree
    --path <PATH>                                 #   anchor a blob path instead of the whole revision, resolved relative to cwd (a git pathspec)
    -L, --lines <START,END[:PATH]>                #   a 1-based inclusive line range: START,END / START,+COUNT / a single line
                                                   #   number; a trailing :PATH supplies --path in the same token (git log -L's
                                                   #   grammar). Requires a path from either source.
    --worktree                                    #   anchor --path's on-disk content instead of a revision; requires --path,
                                                   #   conflicts with --at
    --json <VALUE>                                #   a whole facet_value::Value JSON literal for the document; conflicts with
                                                   #   <text>. The binding field is always injected from the capture pipeline,
                                                   #   overriding anything this literal sets there.
git anchor list <kind> [--json]                   # every entity of <kind>, name plus value
git anchor show <kind> <name> [--json]            # one entity by its full name, as printed by add/list
git anchor show <kind> <name>@<rev> [--json]      # project a position binding onto another revision
git anchor show <kind> <name> --worktree [--json] # project a position binding onto the working tree
git anchor remove <kind> <name>...  (alias: rm)   # delete one or more entities, all checked to exist before any is removed
```

`--prefix <PREFIX>` (default `refs/anchors`) is a global option selecting the store's ref namespace — pass `refs/comments` to reach `gix-comment`'s kinds, or any other prefix a `gix-store` consumer publishes under.
`<name>` is an entity's full name, `<target-hex>/<id-hex>`, exactly as `add`/`list` printed it.

`add`'s one remaining rule, beyond the binding field: among the kind's other required fields, exactly one whose shape is `Node::String` is filled from `<text>`; zero or more than one refuses with an error naming the candidates.
A required field neither the binding nor `<text>` can fill refuses `add` outright, naming the fields — `--json` is the escape hatch that supplies the whole document explicitly.

## How it works

`git anchor` is a thin CLI over [`gix-store`](https://github.com/git-ents/git-store)'s dynamic (schema-only) read/write path: `add` fetches `<kind>`'s published `Schema`, locates the field structurally equal to `Binding`'s own schema, and writes a `facet_value::Value` conforming to it — never a compiled Rust type.
`show <name>@<rev>` and `show <name> --worktree` re-derive where a position binding sits elsewhere, exactly as [`gix-anchor`](../gix-anchor)'s `project`/`project_worktree` always did; they operate on the `Binding` extracted from the read entity, not on any document-specific field.
The document embeds the anchor's tree inline, so the anchored content stays reachable — no gitlinks, no copies.
