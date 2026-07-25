# git-comment

Pin a message to a Git object — a commit, or a blob path and an optional line range within it — as a comment that *follows the content* across history.
Comments live in the repository as real refs and commits, with `git notes`-style semantics: one editable comment per anchored target, versioned for free.
A comment's author and timestamp are never typed in by hand; they are read back from the storage commit itself, exactly as `git log` reports them, so `list`/`show`/`log` always attribute a comment to whoever actually wrote that version, when.
A comment can also carry an optional raw-tree attachment — arbitrary content hung alongside the message, kept reachable through the comment's own ref.

The binary is named `git-comment`, so git's external-subcommand dispatch makes `git comment …` work with nothing more than `PATH`.

## Demo

```console
$ git comment add --path src/lib.rs -L 10,14 -m "revisit this bound"
dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b

$ git comment list
dd1ebeb2  Ada  revisit this bound

$ git comment show dd1ebeb2            # a hex prefix resolves, like git
id: dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b
target: 7a28df3c975fa62270a452251c4e0b24d685c4ba
binding: position
author: Ada <ada@example.com>
date: 2024-05-01T12:00:00+00:00
message:
revisit this bound
snippet:
    fn resolve(&self) -> Result<Id> {
    …

$ git comment show dd1ebeb2@main       # where does that span sit on main?
relocated
path: src/lib.rs
lines: 12,16

$ git comment log dd1ebeb2             # every version, newest first
a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2 2024-05-01T12:00:00+00:00 Ada revisit this bound

$ git comment remove dd1ebeb2
```

Attach to a whole commit instead of a line range by omitting `--path`, and carry an arbitrary tree alongside the message with `--attach`:

```console
git comment add HEAD -m "reviewed, ship it" --attach HEAD^{tree}
```

## Commands

```text
git comment                                 # bare invocation: same as `list`
git comment add [<object>]                  # attach a comment; <object> defaults to HEAD
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
git comment edit <id>                       # replace a comment's message ($EDITOR seeded with the current one)
    -m, --message <MSG> | -F, --file <FILE>
    --attach <TREE-ISH>                     #   replace the attachment; omit to keep the existing one
git comment append <id>                     # join new content onto the existing message with a blank line
    -m, --message <MSG> | -F, --file <FILE>
git comment list [<object>] [--json]  (alias: ls)
                                            # every comment, or those attached to <object> — including a
                                            # position comment whose anchor's own commit is <object>. Use
                                            # <rev>:<path> as <object> for the blob-precise form (only the
                                            # comment(s) anchored to that exact blob).
git comment show <id> [--json] [--worktree] # a comment's target, binding, author, message, and snippet
git comment show <id>@<rev> [--json]        # project a position-bound comment onto <rev>
git comment show <id>~N | <id>^ [--json]    # an older version of the comment itself (~0/bare id is the tip)
git comment log <id>                        # a comment's version history: <oid> <iso-date> <author> <summary>, newest first
git comment remove <id>...    (alias: rm)   # delete one or more comments, resolved atomically before any removal
```

`<id>` is a comment's identity object id; any unambiguous hex prefix resolves, the same way git resolves short revisions.
`--path` (and `-L`'s embedded `:PATH`) is resolved relative to the current directory, like any git pathspec — not the repository root.
`--attach <TREE-ISH>` resolves to a tree the same way `git` does — a commit, a tag, or a tree itself all peel down to the tree that gets embedded.

Writing is always an explicit `add`, and reads default to listing — the same shape as `git remote`/`git notes`; bare `git comment` lists everything.
At a terminal with no `-m`/`-F` and nothing piped, `add` opens `$EDITOR` for the message (seeded with the current message for `edit`), like `git notes add`/`git notes edit`.

## How it works

`git comment` is a thin CLI over the [`gix-comment`](../gix-comment) library, which itself is a thin, opinionated view over [`gix-anchor`](../gix-anchor)'s note store: `add` *captures* an anchor (blob + optional line range + commit) or names a commit directly, and stores the message with it under `refs/anchors/<target>/<binding-id>`, exactly as `git-anchor` stores a note.
What makes it a *comment* rather than a bare note is that the author and timestamp are never separate fields — they are read back from the storage commit's own author signature, since a note is a git commit and git already records who wrote it and when.
An optional `--attach`ed tree rides along in the same commit, so it stays reachable — no gitlinks, no copies.
`show <id>@<rev>` re-derives where the anchored span sits on another commit, reporting *current*, *relocated*, *outdated*, or *deleted*; `show <id> --worktree` re-derives the same against the on-disk working tree instead of a commit.
