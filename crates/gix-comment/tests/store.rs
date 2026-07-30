//! Integration tests for [`Comments`]: the ref-based comment persistence
//! layer, exercised against a real `gix::Repository` rather than the
//! in-memory doubles `crates/gix-comment/src/store.rs`'s unit tests use —
//! add/get round-trip, target filtering, edit history, removal, and
//! `anchor.retention` (the anchored blob stays reachable through the
//! comment's own tree).
//!
//! `gix-comment` is genesis-keyed only: every [`Comments::add`]/
//! [`Comments::reply`] mints a fresh identity, never derived from a
//! [`Binding`]. There is no binding-keyed "re-attach onto the same ref";
//! [`Comments::edit`] is the versioning path a genesis-keyed identity has
//! instead.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::process::Command;

use gix_comment::{Binding, Comments, Error, State};

fn repo(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    };
    run(&["init", "-q"]);
    // Persisted in the repo config (not just `-c` on one command), so gix's
    // own commit path — which writes the *comment* commit — resolves a
    // committer identity too.
    run(&["config", "user.name", "test"]);
    run(&["config", "user.email", "test@example.com"]);
    std::fs::write(dir.path().join("file.txt"), content).unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "one"]);
    dir
}

fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
    range.map(|n| format!("line {n}\n")).collect()
}

fn head_commit(repo: &gix::Repository) -> gix::ObjectId {
    repo.head_id().expect("head").detach()
}

/// Adding a comment and reading it back recovers the exact message and
/// binding, keyed by [`Binding::target`].
#[test]
fn add_and_get_round_trip_body_and_binding() {
    let dir = repo(&numbered(1..=10));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let anchor = gix_comment::capture(
        &git_repo,
        "HEAD",
        "file.txt",
        Some(gix_comment::LineRange { start: 3, end: 4 }),
    )
    .unwrap();
    let binding = Binding::Position(anchor);

    let id = comments
        .add(&binding, "these two lines look off", None)
        .unwrap();
    let comment = comments.get(id).unwrap().expect("comment exists");

    assert_eq!(comment.id, id);
    assert_eq!(comment.target, binding.target());
    assert_eq!(comment.binding, binding);
    assert_eq!(comment.message, "these two lines look off");
}

/// A comment on a `Binding::Commit` round-trips identically. The deleted
/// `gix-anchor` predecessor of this test also checked a custom commit
/// *message* overriding a default summary — `Store::attach` took a
/// `Option<&str>` message parameter. `Comments::add` has no such parameter:
/// the storage commit's summary is always derived from the message body
/// (`summary_of`), and `Comment` does not expose that summary back to a
/// caller at all. That property has no surviving public-API equivalent, so
/// it is dropped rather than ported.
#[test]
fn add_to_a_commit_binding_round_trips_target_and_body() {
    let dir = repo(&numbered(1..=5));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let commit = head_commit(&git_repo);
    let binding = Binding::Commit {
        commit: commit.into(),
    };

    let id = comments.add(&binding, "looks good", None).unwrap();
    let comment = comments.get(id).unwrap().expect("comment exists");

    assert_eq!(comment.binding, binding);
    assert_eq!(comment.target, commit);
    assert_eq!(comment.message, "looks good");
}

/// `get` returns `None` for an id nothing was ever added under.
#[test]
fn get_returns_none_for_an_unknown_id() {
    let dir = repo(&numbered(1..=3));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
    assert!(comments.get(bogus).unwrap().is_none());
}

/// `list(Some(target))` only returns comments filed under that target;
/// `list(None)` returns every comment across every target.
#[test]
fn list_filters_by_target() {
    let dir = repo(&numbered(1..=10));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let commit = head_commit(&git_repo);
    let commit_binding = Binding::Commit {
        commit: commit.into(),
    };
    let anchor_binding =
        Binding::Position(gix_comment::capture(&git_repo, "HEAD", "file.txt", None).unwrap());

    let commit_id = comments
        .add(&commit_binding, "a commit comment", None)
        .unwrap();
    let anchor_id = comments
        .add(&anchor_binding, "an anchor comment", None)
        .unwrap();

    let commit_comments = comments.list(Some(commit_binding.target())).unwrap();
    assert_eq!(commit_comments.len(), 1);
    assert_eq!(commit_comments[0].id, commit_id);

    let anchor_comments = comments.list(Some(anchor_binding.target())).unwrap();
    assert_eq!(anchor_comments.len(), 1);
    assert_eq!(anchor_comments[0].id, anchor_id);

    let mut all_ids: Vec<_> = comments
        .list(None)
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    all_ids.sort();
    let mut expected = vec![commit_id, anchor_id];
    expected.sort();
    assert_eq!(all_ids, expected);
}

/// `gix-comment` has no binding-keyed "re-attach onto the same ref" —
/// `Comments::add` always mints a fresh genesis identity, even for a
/// repeated binding (see `add_mints_distinct_ids_and_edit_versions_one_forward_by_id`
/// below). `Comments::edit` is the surviving versioning path for *one*
/// identity: it commits a new version forward onto the same comment and
/// records both versions in history, tip-first — the property the deleted
/// `reattach_records_history_on_the_same_note` was checking.
#[test]
fn edit_records_history_on_the_same_comment() {
    let dir = repo(&numbered(1..=5));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let commit = head_commit(&git_repo);
    let binding = Binding::Commit {
        commit: commit.into(),
    };

    let id = comments.add(&binding, "first version", None).unwrap();
    let id2 = comments.edit(id, "second version", None).unwrap();
    assert_eq!(id, id2, "edit preserves the genesis identity");

    let history = comments.history(id).unwrap();
    assert_eq!(
        history.len(),
        2,
        "two commits recorded on the comment's ref"
    );

    let latest = comments.get(id).unwrap().expect("comment exists");
    assert_eq!(latest.message, "second version");
}

/// `history` is empty and `remove` returns `false` for a comment that was
/// never added.
#[test]
fn history_and_remove_on_an_absent_note() {
    let dir = repo(&numbered(1..=3));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
    assert!(comments.history(bogus).unwrap().is_empty());
    assert!(!comments.remove(bogus).unwrap());
}

/// `remove` deletes the comment's ref, returning whether it existed; the
/// comment is gone afterward and a second `remove` reports `false`.
#[test]
fn remove_deletes_the_note_and_reports_whether_it_existed() {
    let dir = repo(&numbered(1..=5));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let commit = head_commit(&git_repo);
    let binding = Binding::Commit {
        commit: commit.into(),
    };
    let id = comments.add(&binding, "note body", None).unwrap();

    assert!(comments.remove(id).unwrap());
    assert!(comments.get(id).unwrap().is_none());
    assert!(!comments.remove(id).unwrap());
}

/// An attachment tree round-trips: `add` records it, `get` reports its oid,
/// and it is reachable by walking the comment's own committed tree — the
/// same `anchor.retention` guarantee the binding's own blobs get. The plain
/// `add` leaves the attachment `None`, and the comment's `commit` field is a
/// real commit.
#[test]
fn attachment_round_trips_and_stays_reachable() {
    let dir = repo(&numbered(1..=5));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let commit = head_commit(&git_repo);
    let binding = Binding::Commit {
        commit: commit.into(),
    };

    // Use HEAD's own tree as an arbitrary "raw tree" attachment.
    let attach_tree = git_repo
        .find_commit(commit)
        .unwrap()
        .tree_id()
        .unwrap()
        .detach();
    let id = comments
        .add(&binding, "see the attached tree", Some(attach_tree))
        .unwrap();
    let comment = comments.get(id).unwrap().expect("comment exists");
    assert_eq!(comment.attachment, Some(attach_tree));
    assert!(
        git_repo.find_commit(comment.commit).is_ok(),
        "commit field is real"
    );

    // The plain add path leaves the attachment absent.
    let plain = comments
        .add(
            &Binding::Position(gix_comment::capture(&git_repo, "HEAD", "file.txt", None).unwrap()),
            "no attachment",
            None,
        )
        .unwrap();
    assert_eq!(comments.get(plain).unwrap().unwrap().attachment, None);

    // The attachment tree is reachable from the comment's own committed
    // tree, walked from the public `commit` field rather than a hardcoded
    // ref name.
    let tree = git_repo
        .find_commit(comment.commit)
        .unwrap()
        .tree_id()
        .unwrap()
        .detach();
    let mut stack = vec![tree];
    let mut found = false;
    while let Some(id) = stack.pop() {
        if id == attach_tree {
            found = true;
            break;
        }
        for entry in git_repo.find_tree(id).unwrap().iter() {
            let entry = entry.unwrap();
            if entry.mode().is_tree() {
                stack.push(entry.object_id());
            }
        }
    }
    assert!(
        found,
        "the attachment tree must be reachable from the comment's own tree"
    );
}

/// `add` mints a fresh genesis identity every call, even for the same
/// binding — two comments about one binding never collide onto one ref,
/// unlike a binding-keyed identity. `edit` then versions one of them
/// forward by id, leaving the other untouched, and carries the binding
/// forward unchanged.
#[test]
fn add_mints_distinct_ids_and_edit_versions_one_forward_by_id() {
    let dir = repo(&numbered(1..=5));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let commit = head_commit(&git_repo);
    let binding = Binding::Commit {
        commit: commit.into(),
    };

    let first = comments.add(&binding, "first note", None).unwrap();
    let second = comments.add(&binding, "second note", None).unwrap();
    assert_ne!(first, second, "same binding, distinct genesis identities");

    let first_comment = comments.get(first).unwrap().expect("first exists");
    assert_eq!(first_comment.message, "first note");
    assert_eq!(first_comment.binding, binding);
    assert_eq!(first_comment.parent, None);
    assert_eq!(first_comment.state, State::Open);

    let updated = comments.edit(first, "first note, edited", None).unwrap();
    assert_eq!(updated, first, "edit preserves the genesis identity");

    let latest = comments.get(first).unwrap().expect("first still exists");
    assert_eq!(latest.message, "first note, edited");
    assert_eq!(latest.binding, binding, "binding carried forward unchanged");

    let history = comments.history(first).unwrap();
    assert_eq!(history.len(), 2, "add + edit recorded on one ref");

    // The second comment is untouched by editing the first.
    let second_comment = comments.get(second).unwrap().expect("second exists");
    assert_eq!(second_comment.message, "second note");
    assert_eq!(second_comment.state, State::Open);
}

/// `edit` on an id nothing was ever `add`ed under fails with
/// `Error::Resolve` rather than silently creating a comment.
#[test]
fn edit_of_a_missing_id_errors() {
    let dir = repo(&numbered(1..=3));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
    let result = comments.edit(bogus, "x", None);
    assert!(matches!(result, Err(Error::Resolve(_))));
}

// `with_prefix_roots_a_separate_namespace`, the deleted file's twelfth
// test, has no port here: `gix-anchor::Store::with_prefix` let a caller
// root the same engine at a different ref namespace, but `Comments::open`
// hardcodes `refs/comments` with no prefix parameter, and the internal
// `store::Store::with_prefix` it delegates to is `pub(crate)` — invisible
// to this file, which links against `gix-comment` as an external crate and
// sees only its public API. The property this test checked (two stores at
// different prefixes are mutually invisible) has no surviving public
// surface to exercise; widening `Comments`'s public API to add one back
// would go beyond porting existing coverage.

/// `anchor.retention`: the retained content leaf stays reachable by walking
/// the comment's own committed tree — the store's ref keeps it alive
/// through force-push, branch deletion, and gc with no special-casing.
/// `crates/gix-anchor/tests/retention.rs` checks this same guarantee at the
/// bare `Anchor`/`Binding` serialization level; this test checks it one
/// layer up, through the actually-stored [`Comment`](gix_comment::Comment)
/// and a real repository's refs and object database, which nothing else in
/// this crate currently exercises.
#[test]
fn anchored_content_is_reachable_from_the_notes_own_tree() {
    let dir = repo(&numbered(1..=10));
    let git_repo = gix::open(dir.path()).unwrap();
    let comments = Comments::open(&git_repo);

    let anchor = gix_comment::capture(&git_repo, "HEAD", "file.txt", None).unwrap();
    let retained_content = {
        let memory = facet_git_tree::ObjectStore::default();
        let tree = facet_git_tree::serialize_into(&anchor, &memory).expect("serialize anchor");
        memory
            .get_tree(&tree)
            .expect("anchor tree")
            .into_iter()
            .find(|entry| entry.filename == "content")
            .expect("content entry")
            .oid
    };
    let binding = Binding::Position(anchor);

    let id = comments.add(&binding, "reachability check", None).unwrap();
    let comment = comments.get(id).unwrap().expect("comment exists");

    // Walk from the comment's own storage commit — the same commit its ref
    // points at — never a hardcoded ref name.
    let tree = git_repo
        .find_commit(comment.commit)
        .expect("commit field is real")
        .tree_id()
        .expect("tree")
        .detach();

    let mut stack = vec![tree];
    let mut found = false;
    while let Some(id) = stack.pop() {
        let tree_obj = git_repo.find_tree(id).expect("tree object");
        for entry in tree_obj.iter() {
            let entry = entry.expect("tree entry");
            if entry.mode().is_tree() {
                stack.push(entry.object_id());
            } else if entry.object_id() == retained_content {
                found = true;
            }
        }
    }
    assert!(
        found,
        "the retained content blob must be reachable from the comment's own tree"
    );
}
