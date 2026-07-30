# git-anchor

Attach arbitrary content to Git objects — a commit, or a blob path and an optional line range within it — as a note that *follows the content* across history.
Notes live in the repository as real refs and commits, with `git notes`-style semantics: one editable note per anchored target, versioned for free.

The binary is named `git-anchor`, so git's external-subcommand dispatch makes `git anchor …` work with nothing more than `PATH`.

## Demo

```console
$ git anchor add --path src/lib.rs -L 10,14 -m "revisit this bound"
dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b

$ git anchor list
dd1ebeb2  7a28df3c  revisit this bound

$ git anchor show dd1ebeb2            # a hex prefix resolves, like git
id: dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b
target: 7a28df3c975fa62270a452251c4e0b24d685c4ba
binding: position
body:
revisit this bound
snippet:
    fn resolve(&self) -> Result<Id> {
    …

$ git anchor show dd1ebeb2@main       # where does that span sit on main?
relocated
path: src/lib.rs
lines: 12,16

$ git anchor log dd1ebeb2             # every version, newest first
a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2 2024-05-01T12:00:00+00:00 anchor 7a28df3c975fa62270a452251c4e0b24d685c4ba

$ git anchor remove dd1ebeb2
```

Attach to a whole commit instead of a line range by omitting `--path`:

```console
git anchor add HEAD -m "reviewed, ship it"
```

## Commands

```text
git anchor                                  # bare invocation: same as `list`
git anchor add [<object>]                   # attach a note; <object> defaults to HEAD
    --path <PATH>                           #   anchor a blob path, resolved relative to cwd (a git pathspec)
    -L, --lines <START,END[:PATH]>          #   a 1-based inclusive line range: START,END / START,+COUNT / a
                                            #   single line number; a trailing :PATH supplies --path in the
                                            #   same token (git log -L's grammar). Requires a path from
                                            #   either source.
    --worktree                              #   anchor --path's on-disk content instead of a revision;
                                            #   conflicts with <object>
    -m, --message <MSG>                     #   note body, taken verbatim
    -F, --file <FILE>                       #   read the body from a file
                                            #   (else piped stdin, else $VISUAL/$EDITOR)
git anchor edit <id>                        # replace a note's body ($EDITOR seeded with the current body)
    -m, --message <MSG> | -F, --file <FILE>
git anchor append <id>                      # join new content onto the existing body with a blank line
    -m, --message <MSG> | -F, --file <FILE>
git anchor list [<object>] [--json]  (alias: ls)
                                            # every note, or those attached to <object> — including a
                                            # position note whose anchor's own commit is <object>. Use
                                            # <rev>:<path> as <object> for the blob-precise form (only the
                                            # note(s) anchored to that exact blob).
git anchor show <id> [--json] [--worktree]  # a note's target, binding, body, and snippet
git anchor show <id>@<rev> [--json]         # project a position-bound note onto <rev>
git anchor show <id>~N | <id>^ [--json]     # an older version of the note itself (~0/bare id is the tip)
git anchor log <id>                         # a note's version history: <oid> <iso-date> <summary>, newest first
git anchor remove <id>...    (alias: rm)    # delete one or more notes, resolved atomically before any removal
```

`<id>` is a note's identity object id; any unambiguous hex prefix resolves, the same way git resolves short revisions.
`--path` (and `-L`'s embedded `:PATH`) is resolved relative to the current directory, like any git pathspec — not the repository root.

Writing is always an explicit `add`, and reads default to listing — the same shape as `git remote`/`git notes`; bare `git anchor` lists everything.
At a terminal with no `-m`/`-F` and nothing piped, `add` opens `$EDITOR` for the body (seeded with the current body for `edit`), like `git notes add`/`git notes edit`.

## How it works

`git anchor` is a thin CLI over the [`gix-anchor`](../gix-anchor) library: `add` *captures* an anchor (blob + optional line range + commit) and stores it with the body under `refs/anchors/data/notes/<target>/<binding-id>`; `show <id>@<rev>` re-derives where the anchored span sits on another commit, reporting *current*, *relocated*, *outdated*, or *deleted*; `show <id> --worktree` re-derives the same against the on-disk working tree instead of a commit.
The note embeds the anchor's tree by object id, so the anchored content stays reachable — no gitlinks, no copies.
