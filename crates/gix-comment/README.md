# gix-comment

Comments attached to Git objects, built on [`gix-anchor`](../gix-anchor).

A comment is the simplest client of an anchor: a message pinned to whatever a `Binding` names — a commit, a tree, or a durable line range in a blob — that *follows the content* across history exactly as the anchor it rides on does.
This crate adds no persistence of its own; it is a thin view over `gix-anchor`'s note store.

- The comment **message** is the note body.
- The comment **author** and **timestamp** are the storage commit's — a note is a git commit, so git already records who wrote it and when, and this crate reads those back rather than storing them twice.
- An optional **attachment** is an arbitrary tree, embedded so it stays reachable through the comment's own ref.
- A comment can be **replied to** (`reply`), joining a **thread** (`thread`) rather than colliding with the comment it is about.
- A comment has a resolvable **state** (`open`/`resolved`), flipped by `resolve`/`reopen` without touching its message.

## Example

```rust
use gix_comment::{Binding, Comments, State};

let repo = gix::open(".")?;
let comments = Comments::open(&repo);

// Comment on the current commit.
let head = repo.head_id()?.detach();
let id = comments.add(&Binding::Commit { commit: head.into() }, "ship it", None)?;

// Read it back — the author is whoever git recorded on the storage commit.
let comment = comments.get(id)?.expect("exists");
assert_eq!(comment.message, "ship it");
assert_eq!(comment.state, State::Open);
println!("{} <{}> at {:?}", comment.author.name, comment.author.email, comment.author.time);

// Reply to it — a new comment, sharing the same binding, linked by `parent`.
let reply_id = comments.reply(id, "agreed", None)?;
let thread = comments.thread(id)?;
assert_eq!(thread.root.id, id);
assert_eq!(thread.replies[0].id, reply_id);

// Mark it resolved, then reopen it — the message never changes.
comments.resolve(id)?;
assert_eq!(comments.get(id)?.unwrap().state, State::Resolved);
comments.reopen(id)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## API

- `Comments::open(&repo)` — a comment store over a `gix` repository.
- `add(&binding, message, attachment)` — start a new thread; every call creates a distinct comment, even on a repeated binding.
- `reply(parent_id, message, attachment)` — reply to an existing comment, inheriting its binding and joining its thread.
- `thread(id)` — the whole thread `id` belongs to: its root plus every reply, oldest first.
- `list(target)` / `list_roots(target, include_resolved)` / `get(id)` — read every comment, only thread roots, or one by id.
- `edit(id, message, attachment)` — replace a comment's message, versioning forward; `parent` and `state` carry over unchanged.
- `resolve(id)` / `reopen(id)` — flip a comment's `State` forward, message and attachment untouched.
- `history(id)` / `get_at(id, commit)` — a comment's version ids (newest first), and any past version.
- `remove(id)` — delete a comment.

Everything needed to describe what a comment is *about* — `Binding`, `capture`, `capture_worktree`, `LineRange`, and projection — is re-exported, so consumers depend on `gix-comment` alone.

## How it works

Every comment is a note under `refs/comments/data/notes/<target>/<id>`, committed with the repository's configured identity — its own namespace, separate from `gix-anchor`'s `refs/anchors`, though both run on the same underlying store.
Unlike a plain anchor note, a comment's identity is *not* derived from its binding: `add` and `reply` mint a fresh id — the oid of the parentless commit each writes — every time they are called.
That is what makes threads possible: a reply is about the same binding as the comment it replies to, and two people can comment on the same line, so binding identity alone cannot tell separate comments apart.
`Comment::parent` records which comment (by id) a reply is about, `None` for a thread's root; `thread` walks that link to gather a root and every comment whose own parent chain leads back to it.
`Comment::state` is likewise a plain opaque string under the hood (`"open"`/`"resolved"`), read back into this crate's own two-value `State`.
The message is the note body; the author and time are read back from the storage commit; the attachment, when present, is embedded in the note's own tree so it survives gc.
`edit`/`resolve`/`reopen` all commit a new version forward onto the same ref (by id, not by binding), so a comment carries its full edit and state history for free.
