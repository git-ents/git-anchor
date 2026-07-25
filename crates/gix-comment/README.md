# gix-comment

Comments attached to Git objects, built on [`gix-anchor`](../gix-anchor).

A comment is the simplest client of an anchor: a message pinned to whatever a `Binding` names — a commit, a tree, or a durable line range in a blob — that *follows the content* across history exactly as the anchor it rides on does.
This crate adds no persistence of its own; it is a thin view over `gix-anchor`'s note store.

- The comment **message** is the note body.
- The comment **author** and **timestamp** are the storage commit's — a note is a git commit, so git already records who wrote it and when, and this crate reads those back rather than storing them twice.
- An optional **attachment** is an arbitrary tree, embedded so it stays reachable through the comment's own ref.

## Example

```rust
use gix_comment::{Binding, Comments};

let repo = gix::open(".")?;
let comments = Comments::open(&repo);

// Comment on the current commit.
let head = repo.head_id()?.detach();
let id = comments.add(&Binding::Commit { commit: head }, "ship it", None)?;

// Read it back — the author is whoever git recorded on the storage commit.
let comment = comments.get(id)?.expect("exists");
assert_eq!(comment.message, "ship it");
println!("{} <{}> at {:?}", comment.author.name, comment.author.email, comment.author.time);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## API

- `Comments::open(&repo)` — a comment store over a `gix` repository.
- `add(&binding, message, attachment)` — add a comment; author and timestamp are the storage commit's.
- `list(target)` / `get(id)` — read every comment, those on one target, or one by id.
- `edit(id, message, attachment)` — replace a comment's message, versioning forward.
- `history(id)` / `get_at(commit)` — a comment's version ids (newest first), and any past version.
- `remove(id)` — delete a comment.

Everything needed to describe what a comment is *about* — `Binding`, `capture`, `capture_worktree`, `LineRange`, and projection — is re-exported, so consumers depend on `gix-comment` alone.

## How it works

Each comment is a note under `refs/anchors/<target>/<id>`, committed with the repository's configured identity.
The message is the note body; the author and time are read back from that commit; the attachment, when present, is embedded in the note's own tree so it survives gc.
Re-adding to the same binding commits a new version forward onto the same ref, so a comment carries its full edit history for free.
