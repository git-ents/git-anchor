# git-anchor

Attach arbitrary content to Git objects — a commit, or a blob path and an optional line range within it — as a note that *follows the content* across history.
Notes live in the repository as real refs and commits, with `git notes`-style semantics: one editable note per anchored target, versioned for free.

The binary is named `git-anchor`, so git's external-subcommand dispatch makes `git anchor …` work with nothing more than `PATH`.

## Demo

```console
$ git anchor add --path src/lib.rs -L 10,14 -m "revisit this bound"
id: dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b

$ git anchor list
dd1ebeb2  7a28df3c  revisit this bound

$ git anchor show dd1ebeb2            # a hex prefix resolves, like git
id: dd1ebeb2e71b2313eeab6b14bf89a7333ac1bd6b
target: 7a28df3c975fa62270a452251c4e0b24d685c4ba
binding: position
message: anchor 7a28df3c975fa62270a452251c4e0b24d685c4ba
body:
revisit this bound
snippet:
    fn resolve(&self) -> Result<Id> {
    …

$ git anchor show dd1ebeb2@main       # where does that span sit on main?
relocated
path: src/lib.rs
lines: 12,16

$ git anchor remove dd1ebeb2
```

Attach to a whole commit instead of a line range by omitting `--path`:

```console
git anchor add HEAD -m "reviewed, ship it"
```

## Commands

```text
git anchor add [<object>]                   # attach a note; <object> defaults to HEAD
    --path <PATH>                           #   anchor a blob path (as it is at <object>)
    -L, --lines <START,END>                 #   a 1-based inclusive line range; requires --path
    -m, --message <MSG>                     #   note body, taken verbatim
    -F, --file <FILE>                       #   read the body from a file
                                            #   (else piped stdin, else $VISUAL/$EDITOR)
git anchor list [<object>]  (alias: ls)     # every note, or those attached to <object>
git anchor show <id> [--json]               # a note's target, binding, body, and snippet
git anchor show <id>@<rev> [--json]         # project a position-bound note onto <rev>
git anchor remove <id>      (alias: rm)     # delete a note
```

`<id>` is a note's identity object id; any unambiguous hex prefix resolves, the same way git resolves short revisions.

Writing is always an explicit `add`, and reads default to listing — the same shape as `git remote`/`git notes`.
At a terminal with no `-m`/`-F` and nothing piped, `add` opens `$EDITOR` for the body, like `git notes add`.

## How it works

`git anchor` is a thin CLI over the [`gix-anchor`](../gix-anchor) library: `add` *captures* an anchor (blob + optional line range + commit) and stores it with the body under `refs/anchors/<target>/<binding-id>`; `show <id>@<rev>` re-derives where the anchored span sits on another commit, reporting *current*, *relocated*, *outdated*, or *deleted*.
The note embeds the anchor's tree by object id, so the anchored content stays reachable — no gitlinks, no copies.
