//! [`Comment`] and [`Comments`]: a thin, opinionated layer over
//! `gix-anchor`'s note [`Store`](gix_anchor::Store).
//!
//! A comment *is* a note. Its message is the note body; its author and
//! timestamp are the storage commit's — a note is a git commit, so the
//! identity and time git already records for every commit are exactly what a
//! comment needs, with nothing stored twice. An optional raw-tree attachment
//! rides along in the note's `attachment` slot, embedded so it stays
//! reachable through the comment's own ref.
//!
//! Every comment is genesis-keyed ([`Store::create`]/[`Store::update`]), not
//! binding-keyed: its identity is its own storage commit's oid, never the
//! binding's. [`Comments::add`] and [`Comments::reply`] therefore always
//! create a *new* comment, even when several comments share a binding — a
//! reply is about the same binding as the comment it replies to, and two
//! people can comment on the same line, so binding identity alone cannot
//! tell those apart. [`Comment::parent`] links a reply to what it replies
//! to, and [`Comments::thread`] walks that link into a tree; [`State`] gives
//! a comment a resolvable lifecycle on top of the same version history
//! [`Comments::edit`] already provides.

use std::collections::{HashMap, HashSet};

use gix::ObjectId;
use gix::bstr::ByteSlice as _;
use gix_anchor::{Binding, RefPrefix, RepoStore, StoredNote};

use crate::error::{Error, Result};

/// Who wrote a comment and when — read back from its storage commit's author
/// signature (`git`'s own `author`/`timestamp`), never stored separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    /// The author's name, as recorded on the storage commit.
    pub name: String,
    /// The author's email, as recorded on the storage commit.
    pub email: String,
    /// When the comment was authored — the storage commit's author time.
    pub time: gix::date::Time,
}

/// A comment's lifecycle state — the store's opaque `state` string
/// ([`gix_anchor::StoredNote::state`]), given a fixed two-value vocabulary at
/// this layer. Anything other than the literal `"resolved"` — including
/// `None`, and any string a future version of this crate does not yet
/// recognize — reads as [`State::Open`], so an old reader never mistakes an
/// unrecognized state for resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Awaiting a response, or simply not (yet) marked resolved.
    Open,
    /// Marked resolved by [`Comments::resolve`].
    Resolved,
}

impl State {
    /// The store's opaque string for this state.
    #[must_use]
    fn as_store_str(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Resolved => "resolved",
        }
    }

    /// Recover a [`State`] from the store's opaque string, defaulting to
    /// [`State::Open`] for `None` or anything unrecognized.
    fn from_store(state: Option<&str>) -> State {
        match state {
            Some("resolved") => State::Resolved,
            _ => State::Open,
        }
    }
}

/// A comment attached to a Git object: a `message`, an [`Author`] (name,
/// email, and time, all from the storage commit), an optional raw-tree
/// `attachment`, the [`Binding`] describing what it is *about* — a commit, a
/// tree, or a durable line-range position in a blob — plus its place in a
/// reply thread and its resolvable [`State`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// The comment's identity oid: its own storage commit's oid, minted
    /// fresh by [`Comments::add`]/[`Comments::reply`] and never derived from
    /// `binding` — so a reply and the comment it replies to, though about
    /// the same binding, always have different ids.
    pub id: ObjectId,
    /// What the comment is filed under — [`Binding::target`].
    pub target: ObjectId,
    /// What the comment is about.
    pub binding: Binding,
    /// The comment text (the note body, decoded lossily as UTF-8).
    pub message: String,
    /// An optional attached tree's oid — arbitrary content hung off the
    /// comment, kept reachable through its ref (`anchor.retention`).
    pub attachment: Option<ObjectId>,
    /// Who authored this version of the comment, and when.
    pub author: Author,
    /// The comment this one is a reply to, when it is one. `None` for a
    /// thread's root — the comment [`Comments::add`] created directly,
    /// never a [`Comments::reply`].
    pub parent: Option<ObjectId>,
    /// Open or resolved. Set by [`Comments::resolve`]/[`Comments::reopen`];
    /// [`Comments::add`]/[`Comments::reply`] start a comment at
    /// [`State::Open`].
    pub state: State,
    /// The storage commit this version of the comment lives at — the ref tip
    /// for [`Comments::get`]/[`Comments::list`], or a past version for
    /// [`Comments::get_at`].
    pub commit: ObjectId,
    /// [`gix_anchor::StoredNote::created_at`]: nanoseconds since the Unix
    /// epoch, best-effort, fixed when the comment was first created and
    /// unchanged by `edit`/`append`/`resolve`/`reopen`. [`Comments::thread`]'s
    /// tiebreaker for two replies whose `author.time` lands in the same
    /// second (git's own resolution); not a display timestamp — use
    /// `author.time` for that.
    pub created_at: u64,
}

/// A comment and its replies, oldest reply first — [`Comments::thread`]'s
/// result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// The thread's root: the comment [`Comments::add`] created, with no
    /// [`Comment::parent`] of its own.
    pub root: Comment,
    /// Every comment whose [`Comment::parent`] chain leads back to `root`,
    /// flattened (not nested) and ordered by author time, ties broken by id
    /// for a total, deterministic order.
    pub replies: Vec<Comment>,
}

/// A store of [`Comment`]s over a `gix` repository, built on
/// `gix-anchor`'s note [`Store`], genesis-keyed under `refs/comments`: every
/// [`Comments::add`]/[`Comments::reply`] call creates a new comment identity,
/// each editable with full version history, each version authored and
/// timestamped by whoever committed it.
pub struct Comments<'r> {
    repo: &'r gix::Repository,
    store: RepoStore<'r>,
}

impl<'r> Comments<'r> {
    /// Open a comment store over `repo`, rooted at `refs/comments` — its own
    /// namespace, distinct from `gix-anchor`'s own `refs/anchors`, though
    /// both share the same underlying [`Store`] engine.
    #[must_use]
    pub fn open(repo: &'r gix::Repository) -> Comments<'r> {
        let prefix = RefPrefix::new("refs/comments").expect("valid ref prefix");
        Comments {
            repo,
            store: RepoStore::with_prefix(repo, prefix),
        }
    }

    /// Start a new thread: a root comment on whatever `binding` names, with
    /// an optional raw-tree `attachment` (a tree-ish already present in the
    /// object database).
    ///
    /// The author and timestamp are not parameters: they are the
    /// repository's configured committer identity and the current time,
    /// recorded by the storage commit itself. Every call creates a distinct
    /// comment with a fresh identity, even when `binding` matches an earlier
    /// call's — two independent root comments can sit on the same binding,
    /// each starting its own thread. [`Comments::reply`] is how a message
    /// joins an *existing* thread instead.
    ///
    /// Returns the new comment's identity oid.
    ///
    /// # Errors
    ///
    /// Propagates a [`Store::create`] failure.
    pub fn add(
        &self,
        binding: &Binding,
        message: &str,
        attachment: Option<ObjectId>,
    ) -> Result<ObjectId> {
        let summary = summary_of(message);
        Ok(self.store.create(
            binding,
            message.as_bytes(),
            attachment,
            None,
            Some(State::Open.as_store_str().to_owned()),
            summary,
        )?)
    }

    /// Reply to an existing comment: a new comment, [`Comment::parent`] set
    /// to `parent_id`, inheriting `parent_id`'s [`Binding`] — a reply is
    /// about the same target its parent is, never a binding of its own.
    ///
    /// Returns the reply's identity oid, distinct from `parent_id`'s.
    ///
    /// # Errors
    ///
    /// [`Error::Resolve`] when `parent_id` names no existing comment.
    /// Otherwise propagates a [`Store::create`] failure.
    pub fn reply(
        &self,
        parent_id: ObjectId,
        message: &str,
        attachment: Option<ObjectId>,
    ) -> Result<ObjectId> {
        let Some(parent) = self.get(parent_id)? else {
            return Err(Error::Resolve(parent_id.to_string()));
        };
        let summary = summary_of(message);
        Ok(self.store.create(
            &parent.binding,
            message.as_bytes(),
            attachment,
            Some(parent_id.to_string()),
            Some(State::Open.as_store_str().to_owned()),
            summary,
        )?)
    }

    /// Every comment, or only those filed under `target`, oldest-id first.
    ///
    /// # Errors
    ///
    /// Propagates a [`Store::list`] failure or a storage-commit read failure
    /// while recovering author and timestamp.
    pub fn list(&self, target: Option<ObjectId>) -> Result<Vec<Comment>> {
        self.store
            .list(target)?
            .into_iter()
            .map(|note| self.hydrate(note))
            .collect()
    }

    /// Every thread root — a comment with no [`Comment::parent`] — under
    /// `target` (every root, when `None`), including a resolved root only
    /// when `include_resolved` is set. Oldest-id first, same order as
    /// [`Comments::list`].
    ///
    /// # Errors
    ///
    /// Propagates a [`Comments::list`] failure.
    pub fn list_roots(
        &self,
        target: Option<ObjectId>,
        include_resolved: bool,
    ) -> Result<Vec<Comment>> {
        Ok(self
            .list(target)?
            .into_iter()
            .filter(|comment| {
                comment.parent.is_none() && (include_resolved || comment.state != State::Resolved)
            })
            .collect())
    }

    /// One comment by its identity oid. `None` when nothing was attached
    /// there. Accepts only a full oid — no prefix resolution.
    ///
    /// # Errors
    ///
    /// Propagates a [`Store::get`] failure or a storage-commit read failure.
    pub fn get(&self, id: ObjectId) -> Result<Option<Comment>> {
        match self.store.get(id)? {
            Some(note) => Ok(Some(self.hydrate(note)?)),
            None => Ok(None),
        }
    }

    /// The full thread `id` belongs to: walk [`Comment::parent`] up to find
    /// the root, then gather every comment whose own parent chain leads back
    /// to that same root — including, when `id` names a reply, sibling
    /// replies `id` did not itself lead to.
    ///
    /// # Errors
    ///
    /// [`Error::Resolve`] when `id` names no existing comment. Otherwise
    /// propagates a [`Comments::list`] failure.
    pub fn thread(&self, id: ObjectId) -> Result<Thread> {
        let Some(start) = self.get(id)? else {
            return Err(Error::Resolve(id.to_string()));
        };

        // Every comment sharing `start`'s target is a candidate — including
        // comments belonging to an entirely separate thread on the same
        // target, filtered back out below by parent-chain membership.
        let by_id: HashMap<ObjectId, Comment> = self
            .list(Some(start.target))?
            .into_iter()
            .map(|comment| (comment.id, comment))
            .collect();

        let root_id = root_of(&by_id, start.id);
        let root = by_id
            .get(&root_id)
            .cloned()
            .unwrap_or_else(|| start.clone());

        let mut replies: Vec<Comment> = by_id
            .values()
            .filter(|comment| comment.id != root.id && root_of(&by_id, comment.id) == root.id)
            .cloned()
            .collect();
        // `created_at` alone already orders replies correctly — it is
        // strictly finer-grained than `author.time`'s one-second resolution
        // — with `id` only as a last-resort tiebreak for the practically
        // unreachable case of two replies landing on the same nanosecond.
        replies.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        Ok(Thread { root, replies })
    }

    /// Read a past version of a comment directly at `commit` — one of the
    /// entries [`Comments::history`] lists — the version-history counterpart
    /// to [`Comments::get`]. Its [`Author`] is that version's author.
    ///
    /// # Errors
    ///
    /// Propagates a [`Store::get_at`] failure or a storage-commit read
    /// failure.
    pub fn get_at(&self, id: ObjectId, commit: ObjectId) -> Result<Comment> {
        let note = self.store.get_at(id, commit)?;
        self.hydrate(note)
    }

    /// Replace a comment's message and/or attachment, committing a new
    /// version forward onto the same comment — its [`Comment::parent`] and
    /// [`Comment::state`] carry over unchanged. The new version is authored
    /// and timestamped afresh. Returns the comment's (unchanged) identity
    /// oid.
    ///
    /// # Errors
    ///
    /// [`Error::Resolve`] when `id` names no existing comment — edit names
    /// an existing comment, unlike [`Comments::add`], which always creates
    /// one. Otherwise propagates a [`Store::update`] failure.
    pub fn edit(
        &self,
        id: ObjectId,
        message: &str,
        attachment: Option<ObjectId>,
    ) -> Result<ObjectId> {
        let Some(existing) = self.get(id)? else {
            return Err(Error::Resolve(id.to_string()));
        };
        let summary = summary_of(message);
        Ok(self.store.update(
            id,
            message.as_bytes(),
            attachment,
            existing.parent.map(|parent| parent.to_string()),
            Some(existing.state.as_store_str().to_owned()),
            summary,
        )?)
    }

    /// Mark a comment resolved, its message and attachment unchanged — a new
    /// version forward, same identity, `state` alone flipped to
    /// [`State::Resolved`]. [`Comments::reopen`] undoes it.
    ///
    /// # Errors
    ///
    /// [`Error::Resolve`] when `id` names no existing comment. Otherwise
    /// propagates a [`Store::update`] failure.
    pub fn resolve(&self, id: ObjectId) -> Result<ObjectId> {
        self.set_state(id, State::Resolved, "resolve")
    }

    /// Mark a resolved comment open again — [`Comments::resolve`]'s inverse,
    /// the same version-forward shape.
    ///
    /// # Errors
    ///
    /// [`Error::Resolve`] when `id` names no existing comment. Otherwise
    /// propagates a [`Store::update`] failure.
    pub fn reopen(&self, id: ObjectId) -> Result<ObjectId> {
        self.set_state(id, State::Open, "reopen")
    }

    /// A comment's version history — storage-commit ids, newest first. Empty
    /// when the comment does not exist.
    ///
    /// # Errors
    ///
    /// Propagates a [`Store::history`] failure.
    pub fn history(&self, id: ObjectId) -> Result<Vec<ObjectId>> {
        Ok(self.store.history(id)?)
    }

    /// Delete a comment. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Propagates a [`Store::remove`] failure.
    pub fn remove(&self, id: ObjectId) -> Result<bool> {
        Ok(self.store.remove(id)?)
    }

    /// [`Comments::resolve`]/[`Comments::reopen`]'s shared implementation:
    /// look `id` up, then version it forward with only `state` changed —
    /// message, attachment, and parent all preserved.
    fn set_state(&self, id: ObjectId, state: State, summary: &str) -> Result<ObjectId> {
        let Some(existing) = self.get(id)? else {
            return Err(Error::Resolve(id.to_string()));
        };
        Ok(self.store.update(
            id,
            existing.message.as_bytes(),
            existing.attachment,
            existing.parent.map(|parent| parent.to_string()),
            Some(state.as_store_str().to_owned()),
            summary,
        )?)
    }

    /// Recover a [`Comment`] from a [`StoredNote`], reading the author and
    /// timestamp from the note's storage commit — the one place a comment's
    /// identity and time come from.
    fn hydrate(&self, note: StoredNote) -> Result<Comment> {
        let commit = self
            .repo
            .find_commit(note.commit)
            .map_err(|error| Error::Commit(error.to_string()))?;
        let signature = commit
            .author()
            .map_err(|error| Error::Commit(error.to_string()))?;
        let author = Author {
            name: signature.name.to_str_lossy().into_owned(),
            email: signature.email.to_str_lossy().into_owned(),
            // A malformed date is degraded to the epoch rather than failing
            // the whole read: the comment text is still worth showing.
            time: signature.time().unwrap_or_default(),
        };
        let parent = match note.parent {
            Some(hex) => Some(
                ObjectId::from_hex(hex.as_bytes()).map_err(|_error| Error::InvalidParent(hex))?,
            ),
            None => None,
        };
        Ok(Comment {
            id: note.id,
            target: note.target,
            binding: note.binding,
            message: String::from_utf8_lossy(&note.body).into_owned(),
            attachment: note.attachment,
            author,
            parent,
            state: State::from_store(note.state.as_deref()),
            commit: note.commit,
            created_at: note.created_at,
        })
    }
}

/// The message's first non-empty line, used as the storage commit's summary
/// so a comment is self-describing in plain git (`git log --oneline
/// refs/comments/…`) — falling back to a fixed placeholder on the
/// (practically unreachable, since a real comment has real content) chance
/// every line is blank, since [`gix_anchor::Store::create`] and
/// [`gix_anchor::Store::update`] require a summary rather than defaulting
/// one themselves.
fn summary_of(message: &str) -> &str {
    message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("comment")
}

/// Walk `id`'s [`Comment::parent`] chain within `by_id` to the id it
/// terminates at — either a comment with no parent, or a parent id that is
/// not itself in `by_id` (a different thread's candidate set, or an id
/// [`Comments::remove`] deleted). [`Comments::thread`]'s grouping key: two
/// comments in the same candidate set belong to the same thread iff this
/// resolves to the same id for both.
fn root_of(by_id: &HashMap<ObjectId, Comment>, id: ObjectId) -> ObjectId {
    let mut current = id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current) {
            // A parent cycle should never occur (a reply's parent always
            // pre-exists it), but this guards against looping forever if
            // storage were ever corrupted into one.
            return current;
        }
        match by_id.get(&current).and_then(|comment| comment.parent) {
            Some(parent) if by_id.contains_key(&parent) => current = parent,
            _ => return current,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "unit test")]

    use std::process::Command;

    use gix_anchor::{Binding, LineRange};

    use super::*;

    fn repo(content: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.com"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        run(&["init", "-q"]);
        // Persist the identity in the repo config (not just `-c` on one
        // command), so gix's own `commit` — which writes the *note* commit,
        // and thus the comment's author — resolves the same Ada identity.
        run(&["config", "user.name", "Ada"]);
        run(&["config", "user.email", "ada@example.com"]);
        std::fs::write(dir.path().join("file.txt"), content).unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "one"]);
        dir
    }

    fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
        range.map(|n| format!("line {n}\n")).collect()
    }

    /// A comment on a commit round-trips message and author, deriving the
    /// author from the storage commit rather than any stored field, and
    /// starts life open with no parent.
    #[test]
    fn comment_on_a_commit_records_message_and_derives_author() {
        let dir = repo(&numbered(1..=5));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let commit = git_repo.head_id().unwrap().detach();
        let id = comments
            .add(&Binding::Commit { commit }, "looks good to me", None)
            .unwrap();

        let comment = comments.get(id).unwrap().expect("exists");
        assert_eq!(comment.message, "looks good to me");
        assert_eq!(comment.target, commit);
        assert_eq!(comment.attachment, None);
        assert_eq!(comment.author.name, "Ada");
        assert_eq!(comment.author.email, "ada@example.com");
        assert!(comment.author.time.seconds > 0, "a real commit time");
        assert_eq!(comment.parent, None);
        assert_eq!(comment.state, State::Open);
    }

    /// A comment on a line-range anchor keeps its position binding, and a
    /// raw-tree attachment round-trips by oid.
    #[test]
    fn comment_on_a_line_range_with_an_attachment() {
        let dir = repo(&numbered(1..=10));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let anchor = gix_anchor::capture(
            &git_repo,
            "HEAD",
            "file.txt",
            Some(LineRange { start: 3, end: 4 }),
        )
        .unwrap();
        let tree = git_repo
            .find_commit(git_repo.head_id().unwrap().detach())
            .unwrap()
            .tree_id()
            .unwrap()
            .detach();

        let id = comments
            .add(&Binding::Position(anchor), "off by one?", Some(tree))
            .unwrap();
        let comment = comments.get(id).unwrap().expect("exists");
        assert!(matches!(comment.binding, Binding::Position(_)));
        assert_eq!(comment.attachment, Some(tree));
    }

    /// Editing commits a new version forward: same id, history grows, and the
    /// latest message wins. `get_at` still reads the older version.
    #[test]
    fn edit_versions_forward_and_history_is_readable() {
        let dir = repo(&numbered(1..=5));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let commit = git_repo.head_id().unwrap().detach();
        let id = comments
            .add(&Binding::Commit { commit }, "first", None)
            .unwrap();
        let id2 = comments.edit(id, "second", None).unwrap();
        assert_eq!(id, id2, "same identity");

        let history = comments.history(id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(comments.get(id).unwrap().unwrap().message, "second");
        // The oldest version is history's tail; reading it recovers "first".
        let oldest = *history.last().unwrap();
        assert_eq!(comments.get_at(id, oldest).unwrap().message, "first");
    }

    /// `edit` on an absent comment resolves to an error, not a silent add.
    #[test]
    fn edit_of_a_missing_comment_errors() {
        let dir = repo(&numbered(1..=3));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
        assert!(matches!(
            comments.edit(bogus, "x", None),
            Err(Error::Resolve(_))
        ));
    }

    /// `remove` deletes a comment and reports whether it existed.
    #[test]
    fn remove_reports_existence() {
        let dir = repo(&numbered(1..=5));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let commit = git_repo.head_id().unwrap().detach();
        let id = comments
            .add(&Binding::Commit { commit }, "delete me", None)
            .unwrap();
        assert!(comments.remove(id).unwrap());
        assert!(comments.get(id).unwrap().is_none());
        assert!(!comments.remove(id).unwrap());
    }

    /// Two calls to `add` on the same binding create two distinct comments —
    /// no collision, unlike the old binding-keyed identity.
    #[test]
    fn add_twice_on_the_same_binding_creates_two_distinct_comments() {
        let dir = repo(&numbered(1..=5));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let commit = git_repo.head_id().unwrap().detach();
        let binding = Binding::Commit { commit };
        let first = comments.add(&binding, "first take", None).unwrap();
        let second = comments.add(&binding, "second take", None).unwrap();
        assert_ne!(first, second);

        let all = comments.list(Some(commit)).unwrap();
        assert_eq!(all.len(), 2);
    }

    /// A reply gets a distinct id, shares the parent's binding, records the
    /// parent link, and starts open.
    #[test]
    fn reply_gets_a_distinct_id_and_shares_the_parents_binding() {
        let dir = repo(&numbered(1..=10));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let anchor = gix_anchor::capture(&git_repo, "HEAD", "file.txt", None).unwrap();
        let binding = Binding::Position(anchor);
        let root_id = comments.add(&binding, "root comment", None).unwrap();

        let reply_id = comments.reply(root_id, "a reply", None).unwrap();
        assert_ne!(reply_id, root_id);

        let reply = comments.get(reply_id).unwrap().expect("exists");
        assert_eq!(reply.parent, Some(root_id));
        assert_eq!(
            reply.binding, binding,
            "reply inherits the parent's binding"
        );
        assert_eq!(reply.state, State::Open);

        // The binding still projects — a reply's position is exactly the
        // root's, so it follows the content the same way.
        assert!(matches!(reply.binding, Binding::Position(_)));
    }

    /// `reply` to a nonexistent comment errors instead of creating an
    /// orphaned reply.
    #[test]
    fn reply_to_a_missing_comment_errors() {
        let dir = repo(&numbered(1..=3));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
        assert!(matches!(
            comments.reply(bogus, "x", None),
            Err(Error::Resolve(_))
        ));
    }

    /// `thread` returns the root plus every reply, oldest first, and two
    /// independent roots on the same binding stay separate threads.
    #[test]
    fn thread_gathers_the_root_and_replies_in_time_order_and_keeps_independent_roots_separate() {
        let dir = repo(&numbered(1..=5));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let commit = git_repo.head_id().unwrap().detach();
        let binding = Binding::Commit { commit };

        let root_a = comments.add(&binding, "root a", None).unwrap();
        let reply_a1 = comments.reply(root_a, "a reply one", None).unwrap();
        let reply_a2 = comments.reply(root_a, "a reply two", None).unwrap();

        // A second, independent root on the very same binding.
        let root_b = comments.add(&binding, "root b", None).unwrap();
        let reply_b1 = comments.reply(root_b, "b reply one", None).unwrap();

        let thread_a = comments.thread(root_a).unwrap();
        assert_eq!(thread_a.root.id, root_a);
        let reply_ids: Vec<_> = thread_a.replies.iter().map(|c| c.id).collect();
        assert_eq!(reply_ids, vec![reply_a1, reply_a2], "oldest reply first");
        assert!(!reply_ids.contains(&reply_b1), "thread a excludes thread b");

        // Asking via a reply's own id still finds the whole thread.
        let via_reply = comments.thread(reply_a2).unwrap();
        assert_eq!(via_reply.root.id, root_a);
        assert_eq!(via_reply.replies.len(), 2);

        let thread_b = comments.thread(root_b).unwrap();
        assert_eq!(thread_b.root.id, root_b);
        assert_eq!(
            thread_b.replies.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![reply_b1]
        );
    }

    /// `thread` on an unknown id errors.
    #[test]
    fn thread_of_a_missing_comment_errors() {
        let dir = repo(&numbered(1..=3));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
        assert!(matches!(comments.thread(bogus), Err(Error::Resolve(_))));
    }

    /// `resolve` then `reopen` version the comment forward: history grows,
    /// the state reads correctly at each tip, the message is untouched, and
    /// who/when still come from the storage commit.
    #[test]
    fn resolve_then_reopen_versions_forward_and_preserves_the_message() {
        let dir = repo(&numbered(1..=5));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let commit = git_repo.head_id().unwrap().detach();
        let id = comments
            .add(&Binding::Commit { commit }, "needs review", None)
            .unwrap();
        assert_eq!(comments.history(id).unwrap().len(), 1);

        let resolved_id = comments.resolve(id).unwrap();
        assert_eq!(resolved_id, id);
        let resolved = comments.get(id).unwrap().expect("exists");
        assert_eq!(resolved.state, State::Resolved);
        assert_eq!(resolved.message, "needs review", "message unchanged");
        assert_eq!(resolved.author.name, "Ada", "author still from the commit");
        assert_eq!(comments.history(id).unwrap().len(), 2);

        let reopened_id = comments.reopen(id).unwrap();
        assert_eq!(reopened_id, id);
        let reopened = comments.get(id).unwrap().expect("exists");
        assert_eq!(reopened.state, State::Open);
        assert_eq!(reopened.message, "needs review");
        assert_eq!(comments.history(id).unwrap().len(), 3);
    }

    /// `resolve`/`reopen` on an absent comment errors.
    #[test]
    fn resolve_and_reopen_of_a_missing_comment_error() {
        let dir = repo(&numbered(1..=3));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
        assert!(matches!(comments.resolve(bogus), Err(Error::Resolve(_))));
        assert!(matches!(comments.reopen(bogus), Err(Error::Resolve(_))));
    }

    /// `list_roots` defaults to open roots only, `include_resolved` brings
    /// resolved roots back, and replies are never listed as roots.
    #[test]
    fn list_roots_defaults_to_open_and_excludes_replies() {
        let dir = repo(&numbered(1..=5));
        let git_repo = gix::open(dir.path()).unwrap();
        let comments = Comments::open(&git_repo);

        let commit = git_repo.head_id().unwrap().detach();
        let binding = Binding::Commit { commit };

        let open_root = comments.add(&binding, "open root", None).unwrap();
        let resolved_root = comments.add(&binding, "resolved root", None).unwrap();
        comments.resolve(resolved_root).unwrap();
        let reply_id = comments.reply(open_root, "a reply", None).unwrap();

        let open_only = comments.list_roots(Some(commit), false).unwrap();
        let open_ids: Vec<_> = open_only.iter().map(|c| c.id).collect();
        assert_eq!(open_ids, vec![open_root]);
        assert!(!open_ids.contains(&reply_id), "a reply is never a root");

        let all_roots = comments.list_roots(Some(commit), true).unwrap();
        let mut all_ids: Vec<_> = all_roots.iter().map(|c| c.id).collect();
        all_ids.sort();
        let mut expected = vec![open_root, resolved_root];
        expected.sort();
        assert_eq!(all_ids, expected);
    }
}
