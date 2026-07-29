//! Integration tests for [`Store`](gix_anchor::Store): the ref-based note
//! persistence layer — attach/get round-trip, target filtering, re-attach
//! history, removal, and `anchor.retention` (the anchored blob stays
//! reachable through the note's own tree).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::process::Command;

use gix_anchor::{Binding, LineRange, Store};

fn fixture_repo(content: &str) -> tempfile::TempDir {
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

/// Attaching a note and reading it back recovers the exact body and
/// binding, keyed by `Binding::target`.
#[test]
fn attach_and_get_round_trip_body_and_binding() {
    let dir = fixture_repo(&numbered(1..=10));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let anchor = gix_anchor::capture(
        &repo,
        "HEAD",
        "file.txt",
        Some(LineRange { start: 3, end: 4 }),
    )
    .unwrap();
    let binding = Binding::Position(anchor);

    let id = store
        .attach(&binding, b"these two lines look off", None)
        .unwrap();
    let note = store.get(id).unwrap().expect("note exists");

    assert_eq!(note.id, id);
    assert_eq!(note.target, binding.target());
    assert_eq!(note.binding, binding);
    assert_eq!(note.body, b"these two lines look off");
    assert_eq!(
        note.message,
        "anchor ".to_owned() + &binding.target().to_string()
    );
}

/// A note attached to a `Binding::Commit` round-trips identically, and a
/// custom message overrides the default summary.
#[test]
fn attach_to_a_commit_binding_with_a_custom_message() {
    let dir = fixture_repo(&numbered(1..=5));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let commit = head_commit(&repo);
    let binding = Binding::Commit { commit };

    let id = store.attach(&binding, b"looks good", Some("lgtm")).unwrap();
    let note = store.get(id).unwrap().expect("note exists");

    assert_eq!(note.binding, binding);
    assert_eq!(note.target, commit);
    assert_eq!(note.body, b"looks good");
    assert_eq!(note.message, "lgtm");
}

/// `get` returns `None` for an id nothing was ever attached to.
#[test]
fn get_returns_none_for_an_unknown_id() {
    let dir = fixture_repo(&numbered(1..=3));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
    assert!(store.get(bogus).unwrap().is_none());
}

/// `list(Some(target))` only returns notes attached to that target; `list(None)`
/// returns every note across every target.
#[test]
fn list_filters_by_target() {
    let dir = fixture_repo(&numbered(1..=10));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let commit = head_commit(&repo);
    let commit_binding = Binding::Commit { commit };
    let anchor_binding =
        Binding::Position(gix_anchor::capture(&repo, "HEAD", "file.txt", None).unwrap());

    let commit_id = store
        .attach(&commit_binding, b"a commit note", None)
        .unwrap();
    let anchor_id = store
        .attach(&anchor_binding, b"an anchor note", None)
        .unwrap();

    let commit_notes = store.list(Some(commit_binding.target())).unwrap();
    assert_eq!(commit_notes.len(), 1);
    assert_eq!(commit_notes[0].id, commit_id);

    let anchor_notes = store.list(Some(anchor_binding.target())).unwrap();
    assert_eq!(anchor_notes.len(), 1);
    assert_eq!(anchor_notes[0].id, anchor_id);

    let mut all_ids: Vec<_> = store
        .list(None)
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    all_ids.sort();
    let mut expected = vec![commit_id, anchor_id];
    expected.sort();
    assert_eq!(all_ids, expected);
}

/// Re-attaching to the same binding commits a new version forward onto the
/// same ref (same identity oid) and records both versions in history,
/// tip-first.
#[test]
fn reattach_records_history_on_the_same_note() {
    let dir = fixture_repo(&numbered(1..=5));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let commit = head_commit(&repo);
    let binding = Binding::Commit { commit };

    let id1 = store.attach(&binding, b"first version", None).unwrap();
    let id2 = store.attach(&binding, b"second version", None).unwrap();
    assert_eq!(id1, id2, "same binding, same identity oid, same ref");

    let history = store.history(id1).unwrap();
    assert_eq!(history.len(), 2, "two commits recorded on the note's ref");

    let latest = store.get(id1).unwrap().expect("note exists");
    assert_eq!(latest.body, b"second version");
}

/// `history` is empty and `remove` returns `false` for a note that was
/// never attached.
#[test]
fn history_and_remove_on_an_absent_note() {
    let dir = fixture_repo(&numbered(1..=3));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
    assert!(store.history(bogus).unwrap().is_empty());
    assert!(!store.remove(bogus).unwrap());
}

/// `remove` deletes the note's ref, returning whether it existed; the note
/// is gone afterward and a second `remove` reports `false`.
#[test]
fn remove_deletes_the_note_and_reports_whether_it_existed() {
    let dir = fixture_repo(&numbered(1..=5));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let commit = head_commit(&repo);
    let binding = Binding::Commit { commit };
    let id = store.attach(&binding, b"note body", None).unwrap();

    assert!(store.remove(id).unwrap());
    assert!(store.get(id).unwrap().is_none());
    assert!(!store.remove(id).unwrap());
}

/// An attachment tree round-trips: `attach_with_attachment` records it,
/// `get` reports its oid, and it is reachable by walking the note's own
/// committed tree — the same `anchor.retention` guarantee the binding's own
/// blobs get. The plain `attach` leaves the attachment `None`, and the note's
/// `commit` field is a real commit whose author/time the store records.
#[test]
fn attachment_round_trips_and_stays_reachable() {
    let dir = fixture_repo(&numbered(1..=5));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let commit = head_commit(&repo);
    let binding = Binding::Commit { commit };

    // Use HEAD's own tree as an arbitrary "raw tree" attachment.
    let attach_tree = repo
        .find_commit(commit)
        .unwrap()
        .tree_id()
        .unwrap()
        .detach();
    let id = store
        .attach_with_attachment(&binding, b"see the attached tree", Some(attach_tree), None)
        .unwrap();
    let note = store.get(id).unwrap().expect("note exists");
    assert_eq!(note.attachment, Some(attach_tree));
    assert!(
        repo.find_commit(note.commit).is_ok(),
        "commit field is real"
    );

    // The plain attach path leaves the attachment absent.
    let plain = store
        .attach(
            &Binding::Position(gix_anchor::capture(&repo, "HEAD", "file.txt", None).unwrap()),
            b"no attachment",
            None,
        )
        .unwrap();
    assert_eq!(store.get(plain).unwrap().unwrap().attachment, None);

    // The attachment tree is reachable from the note's own committed tree.
    let refname = format!("refs/anchors/{}/{}", binding.target(), id);
    let tree = repo
        .find_reference(&refname)
        .unwrap()
        .into_fully_peeled_id()
        .unwrap()
        .object()
        .unwrap()
        .peel_to_tree()
        .unwrap();
    let mut stack = vec![tree.id().detach()];
    let mut found = false;
    while let Some(id) = stack.pop() {
        if id == attach_tree {
            found = true;
            break;
        }
        for entry in repo.find_tree(id).unwrap().iter() {
            let entry = entry.unwrap();
            if entry.mode().is_tree() {
                stack.push(entry.object_id());
            }
        }
    }
    assert!(found, "the attachment tree must be reachable from the note");
}

/// `create` mints a fresh genesis identity every call, even for the same
/// binding — two notes about one binding never collide onto one ref, unlike
/// `attach`. `update` then versions one of them forward by id, leaving the
/// other untouched, and carries the binding forward unchanged.
#[test]
fn create_mints_distinct_ids_and_update_versions_one_forward_by_id() {
    let dir = fixture_repo(&numbered(1..=5));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let commit = head_commit(&repo);
    let binding = Binding::Commit { commit };

    let first = store
        .create(&binding, b"first note", None, None, None, "first")
        .unwrap();
    let second = store
        .create(&binding, b"second note", None, None, None, "second")
        .unwrap();
    assert_ne!(first, second, "same binding, distinct genesis identities");

    let first_note = store.get(first).unwrap().expect("first exists");
    assert_eq!(first_note.body, b"first note");
    assert_eq!(first_note.binding, binding);
    assert_eq!(first_note.parent, None);
    assert_eq!(first_note.state, None);

    let updated = store
        .update(
            first,
            b"first note, edited",
            None,
            Some("some-parent".to_owned()),
            Some("resolved".to_owned()),
            "edit",
        )
        .unwrap();
    assert_eq!(updated, first, "update preserves the genesis identity");

    let latest = store.get(first).unwrap().expect("first still exists");
    assert_eq!(latest.body, b"first note, edited");
    assert_eq!(latest.binding, binding, "binding carried forward unchanged");
    assert_eq!(latest.parent, Some("some-parent".to_owned()));
    assert_eq!(latest.state, Some("resolved".to_owned()));

    let history = store.history(first).unwrap();
    assert_eq!(history.len(), 2, "create + update recorded on one ref");

    // The second note is untouched by updating the first.
    let second_note = store.get(second).unwrap().expect("second exists");
    assert_eq!(second_note.body, b"second note");
    assert_eq!(second_note.state, None);
}

/// `update` on an id nothing was ever `create`d under fails with
/// `Error::Resolve` rather than silently creating a note.
#[test]
fn update_of_a_missing_id_errors() {
    let dir = fixture_repo(&numbered(1..=3));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let bogus = gix::ObjectId::null(gix::hash::Kind::Sha1);
    let result = store.update(bogus, b"x", None, None, None, "edit");
    assert!(matches!(result, Err(gix_anchor::Error::Resolve(_))));
}

/// `Store::with_prefix` roots the same engine at a different ref namespace:
/// notes written through one prefix are invisible to a store opened at
/// another, and vice versa.
#[test]
fn with_prefix_roots_a_separate_namespace() {
    let dir = fixture_repo(&numbered(1..=5));
    let repo = gix::open(dir.path()).unwrap();
    let anchors = Store::open(&repo);
    let comments = Store::with_prefix(&repo, "refs/comments");

    let commit = head_commit(&repo);
    let binding = Binding::Commit { commit };

    let anchor_id = anchors.attach(&binding, b"an anchor note", None).unwrap();
    let comment_id = comments
        .create(&binding, b"a comment", None, None, None, "comment")
        .unwrap();

    assert!(anchors.get(comment_id).unwrap().is_none());
    assert!(comments.get(anchor_id).unwrap().is_none());
    assert_eq!(
        anchors.get(anchor_id).unwrap().unwrap().body,
        b"an anchor note"
    );
    assert_eq!(
        comments.get(comment_id).unwrap().unwrap().body,
        b"a comment"
    );

    let reference = repo
        .find_reference(&format!("refs/comments/{}/{comment_id}", binding.target()))
        .expect("comment ref exists under the custom prefix");
    drop(reference);
}

/// `anchor.retention`: the retained content leaf stays reachable by walking
/// the note's own committed tree — the store's ref keeps it alive through
/// force-push, branch deletion, and gc with no special-casing.
#[test]
fn anchored_content_is_reachable_from_the_notes_own_tree() {
    let dir = fixture_repo(&numbered(1..=10));
    let repo = gix::open(dir.path()).unwrap();
    let store = Store::open(&repo);

    let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).unwrap();
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

    let id = store.attach(&binding, b"reachability check", None).unwrap();

    let refname = format!("refs/anchors/{}/{}", binding.target(), id);
    let reference = repo.find_reference(&refname).expect("note ref exists");
    let commit = reference
        .into_fully_peeled_id()
        .expect("peel")
        .object()
        .expect("object");
    let tree = commit.peel_to_tree().expect("tree");

    let mut stack = vec![tree.id().detach()];
    let mut found = false;
    while let Some(id) = stack.pop() {
        let tree_obj = repo.find_tree(id).expect("tree object");
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
        "the retained content blob must be reachable from the note's own tree"
    );
}
