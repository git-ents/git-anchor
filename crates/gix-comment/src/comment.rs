//! [`Comment`] and [`Comments`]: a thin, opinionated layer over
//! `gix-anchor`'s note [`Store`](gix_anchor::Store).
//!
//! A comment *is* a note. Its message is the note body; its author and
//! timestamp are the storage commit's — a note is a git commit, so the
//! identity and time git already records for every commit are exactly what a
//! comment needs, with nothing stored twice. An optional raw-tree attachment
//! rides along in the note's `attachment` slot, embedded so it stays
//! reachable through the comment's own ref.

use gix::ObjectId;
use gix::bstr::ByteSlice as _;
use gix_anchor::{Binding, Store, StoredNote};

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

/// A comment attached to a Git object: a `message`, an [`Author`] (name,
/// email, and time, all from the storage commit), an optional raw-tree
/// `attachment`, and the [`Binding`] describing what it is *about* — a
/// commit, a tree, or a durable line-range position in a blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// The comment's identity oid — its binding's serialized tree oid, and
    /// the ref-path leaf, exactly as [`StoredNote::id`].
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
    /// The storage commit this version of the comment lives at — the ref tip
    /// for [`Comments::get`]/[`Comments::list`], or a past version for
    /// [`Comments::get_at`].
    pub commit: ObjectId,
}

/// A store of [`Comment`]s over a `gix` repository, built on
/// `gix-anchor`'s note [`Store`]: one comment per [`Binding`] identity,
/// editable with full version history, each version authored and timestamped
/// by whoever committed it.
pub struct Comments<'r> {
    repo: &'r gix::Repository,
    store: Store<'r>,
}

impl<'r> Comments<'r> {
    /// Open a comment store over `repo`.
    #[must_use]
    pub fn open(repo: &'r gix::Repository) -> Comments<'r> {
        Comments {
            repo,
            store: Store::open(repo),
        }
    }

    /// Add a comment to whatever `binding` names, with an optional raw-tree
    /// `attachment` (a tree-ish already present in the object database).
    ///
    /// The author and timestamp are not parameters: they are the
    /// repository's configured committer identity and the current time,
    /// recorded by the storage commit itself. Re-adding to the same binding
    /// commits a new version forward onto the same comment (git-notes style),
    /// so this doubles as the primitive [`Comments::edit`] builds on.
    ///
    /// Returns the comment's identity oid.
    ///
    /// # Errors
    ///
    /// Propagates a [`Store::attach_with_attachment`] failure.
    pub fn add(
        &self,
        binding: &Binding,
        message: &str,
        attachment: Option<ObjectId>,
    ) -> Result<ObjectId> {
        // Use the message's first non-empty line as the storage commit's
        // summary, so the comment is self-describing in plain git
        // (`git log --oneline refs/anchors/…`) — not the store's generic
        // `anchor <target>` default. The full message is still the body.
        let summary = message.lines().find(|line| !line.trim().is_empty());
        Ok(self
            .store
            .attach_with_attachment(binding, message.as_bytes(), attachment, summary)?)
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

    /// Read a past version of a comment directly at `commit` — one of the
    /// entries [`Comments::history`] lists — the version-history counterpart
    /// to [`Comments::get`]. Its [`Author`] is that version's author.
    ///
    /// # Errors
    ///
    /// Propagates a [`Store::get_at`] failure or a storage-commit read
    /// failure.
    pub fn get_at(&self, commit: ObjectId) -> Result<Comment> {
        let note = self.store.get_at(commit)?;
        self.hydrate(note)
    }

    /// Replace a comment's message (and attachment), committing a new version
    /// forward onto the same comment. The new version is authored and
    /// timestamped afresh, exactly as [`Comments::add`] would be. Returns the
    /// comment's (unchanged) identity oid.
    ///
    /// # Errors
    ///
    /// [`Error::Anchor`] with a not-found sense is *not* produced here —
    /// instead a missing `id` yields [`Error::Resolve`], since edit names an
    /// existing comment. Otherwise propagates an attach failure.
    pub fn edit(
        &self,
        id: ObjectId,
        message: &str,
        attachment: Option<ObjectId>,
    ) -> Result<ObjectId> {
        let Some(existing) = self.get(id)? else {
            return Err(Error::Resolve(id.to_string()));
        };
        self.add(&existing.binding, message, attachment)
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
        Ok(Comment {
            id: note.id,
            target: note.target,
            binding: note.binding,
            message: String::from_utf8_lossy(&note.body).into_owned(),
            attachment: note.attachment,
            author,
            commit: note.commit,
        })
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
    /// author from the storage commit rather than any stored field.
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
        assert_eq!(comments.get_at(oldest).unwrap().message, "first");
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
}
