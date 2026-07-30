# git-comment

Pin a message to a Git object — a commit, or a blob path and an optional line range within it — as a comment that *follows the content* across history.
Comments live in the repository as real refs and commits, with `git notes`-style semantics: an editable comment carries its full version history for free, and can be replied to, forming a thread, or marked resolved.
A comment's author and timestamp are never typed in by hand; they are read back from the storage commit itself, exactly as `git log` reports them, so `list`/`show`/`log` always attribute a comment to whoever actually wrote that version, when.
A comment can also carry an optional raw-tree attachment — arbitrary content hung alongside the message, kept reachable through the comment's own ref.

The binary is named `git-comment`, so git's external-subcommand dispatch makes `git comment …` work with nothing more than `PATH`.

## Demo

```console
$ git comment add --path src/lib.rs -L 10,14 -m "revisit this bound"
dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b

$ git comment reply dd1ebeb2 -m "agreed, filed as a follow-up"
9f2c7a1b4e5d6c3a2b1f0e9d8c7b6a5f4e3d2c1b

$ git comment list
dd1ebeb2  Ada  open  (1 reply)  revisit this bound

$ git comment show dd1ebeb2 --thread   # the root, then every reply, oldest first
id: dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b
author: Ada <ada@example.com>
date: 2024-05-01T12:00:00+00:00
state: open
message:
revisit this bound

id: 9f2c7a1b4e5d6c3a2b1f0e9d8c7b6a5f4e3d2c1b
author: Grace <grace@example.com>
date: 2024-05-01T12:05:00+00:00
state: open
message:
agreed, filed as a follow-up

$ git comment show dd1ebeb2            # a hex prefix resolves, like git
id: dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b
target: 7a28df3c975fa62270a452251c4e0b24d685c4ba
binding: position
author: Ada <ada@example.com>
date: 2024-05-01T12:00:00+00:00
state: open
message:
revisit this bound
snippet:
    fn resolve(&self) -> Result<Id> {
    …

$ git comment show dd1ebeb2@main       # where does that span sit on main?
relocated
path: src/lib.rs
lines: 12,16

$ git comment resolve dd1ebeb2         # message and attachment are untouched
dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b

$ git comment log dd1ebeb2             # every version, newest first
a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2 2024-05-01T12:10:00+00:00 Ada revisit this bound
9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e 2024-05-01T12:00:00+00:00 Ada revisit this bound

$ git comment remove dd1ebeb2
```

Attach to a whole commit instead of a line range by omitting `--path`, and carry an arbitrary tree alongside the message with `--attach`:

```console
git comment add HEAD -m "reviewed, ship it" --attach HEAD^{tree}
```

## Commands

```text
git comment                                 # bare invocation: same as `list`
git comment add [<object>]                  # attach a new comment; <object> defaults to HEAD
    --path <PATH>                           #   anchor a blob path, resolved relative to cwd (a git pathspec)
    -L, --lines <START,END[:PATH]>          #   a 1-based inclusive line range: START,END / START,+COUNT / a
                                            #   single line number; a trailing :PATH supplies --path in the
                                            #   same token (git log -L's grammar). Requires a path from
                                            #   either source.
    --worktree                              #   anchor --path's on-disk content instead of a revision;
                                            #   conflicts with <object>
    -m, --message <MSG>                     #   comment message, taken verbatim
    -F, --file <FILE>                       #   read the message from a file
                                            #   (else piped stdin, else $VISUAL/$EDITOR)
    --attach <TREE-ISH>                     #   embed an arbitrary tree-ish's tree alongside the message
git comment reply <id>                      # reply to a comment, inheriting its binding and joining its thread
    -m, --message <MSG> | -F, --file <FILE>
    --attach <TREE-ISH>
git comment edit <id>                       # replace a comment's message ($EDITOR seeded with the current one)
    -m, --message <MSG> | -F, --file <FILE>
    --attach <TREE-ISH>                     #   replace the attachment; omit to keep the existing one
git comment append <id>                     # join new content onto the existing message with a blank line
    -m, --message <MSG> | -F, --file <FILE>
git comment resolve <id>                    # mark a comment resolved; message and attachment unchanged
git comment reopen <id>                     # mark a resolved comment open again
git comment list [<object>] [--json]  (alias: ls)
                                            # open thread roots, or those attached to <object> — including a
                                            # position comment whose anchor's own commit is <object>. Use
                                            # <rev>:<path> as <object> for the blob-precise form (only the
                                            # comment(s) anchored to that exact blob).
    --resolved                              #   include resolved roots alongside open ones
    --all                                   #   list every comment — roots and replies alike, any state
git comment show <id> [--json] [--worktree] # a comment's target, binding, author, message, state, and snippet
git comment show <id> --thread [--json]     # the comment's whole thread: root first, then every reply, oldest first
git comment show <id>@<rev> [--json]        # project a position-bound comment onto <rev>
git comment show <id>~N | <id>^ [--json]    # an older version of the comment itself (~0/bare id is the tip)
git comment log <id>                        # a comment's version history: <oid> <iso-date> <author> <summary>, newest first
git comment remove <id>...    (alias: rm)   # delete one or more comments, resolved atomically before any removal
git comment lsp                             # editor-facing LSP server on stdio (see below); not for interactive use
```

`<id>` is a comment's identity object id; any unambiguous hex prefix resolves, the same way git resolves short revisions.
`--path` (and `-L`'s embedded `:PATH`) is resolved relative to the current directory, like any git pathspec — not the repository root.
`--attach <TREE-ISH>` resolves to a tree the same way `git` does — a commit, a tag, or a tree itself all peel down to the tree that gets embedded.

Writing is always an explicit `add` or `reply`, and reads default to listing — the same shape as `git remote`/`git notes`; bare `git comment` lists open thread roots.
At a terminal with no `-m`/`-F` and nothing piped, `add`/`reply` open `$EDITOR` for the message (seeded with the current message for `edit`), like `git notes add`/`git notes edit`.

## How it works

`git comment` is a thin CLI over the [`gix-comment`](../gix-comment) library, which owns its own `gix-store`-backed document built on [`gix-anchor`](../gix-anchor)'s `Binding`: `add` *captures* an anchor (blob + optional line range + commit) or names a commit directly, and stores the message with it under `refs/comments/data/comment/<target-hex>/<id-hex>` — its own namespace, distinct from `git-anchor`'s default `refs/anchors`, though `git anchor --prefix refs/comments` reaches the same entities generically.
Unlike a plain anchor note, a comment's `<id>` is never derived from its binding: every `add` or `reply` mints a fresh identity, so a reply and the comment it replies to — both about the same binding — never collide onto one ref, and two people can comment on the same line independently.
`reply <id>` records `<id>` as the new comment's parent and inherits its binding automatically; `show <id> --thread` walks that link back to the root and prints the whole thread.
`resolve`/`reopen` commit a new version forward with only the lifecycle state changed, message and attachment untouched — the same version-forward mechanism `edit` uses for content.
What makes it a *comment* rather than a bare note is that the author and timestamp are never separate fields — they are read back from the storage commit's own author signature, since a note is a git commit and git already records who wrote it and when.
An optional `--attach`ed tree rides along in the same commit, so it stays reachable — no gitlinks, no copies.
`show <id>@<rev>` re-derives where the anchored span sits on another commit, reporting *current*, *relocated*, *outdated*, or *deleted*; `show <id> --worktree` re-derives the same against the on-disk working tree instead of a commit.

## Editor integration

`git comment lsp` runs [`gix-comment-lsp`](../gix-comment-lsp) on stdio: a read-time Language Server Protocol view over `refs/comments/*` that projects comments onto whatever buffer an editor has open (code lenses, hover threads, hint diagnostics) and writes new ones back through the same library calls this CLI uses — a comment is one entity everywhere.
See [`editors/zed`](../../editors/zed) for a Zed extension that wires it up.
