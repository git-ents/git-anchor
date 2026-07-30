//! Integration tests for `anchor.retention`: the serialized anchor's hints
//! (fingerprints, descriptors) are ordinary tree entries, reachable from the
//! storing document's tree and never a gitlink — additive, versioned
//! material, never identity-bearing.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::process::Command;

use facet_git_tree::{EntryKind, ObjectStore, RawTree, serialize_into};
use gix_anchor::LineRange;

/// A stand-in for a downstream consumer's `Comment` (this crate cannot
/// depend on such a consumer, which itself depends on this crate): any
/// struct embedding an anchor's tree by [`RawTree`] exercises the same
/// reachability property `anchor.retention` requires.
#[derive(facet::Facet)]
struct Comment {
    body: String,
    anchor: RawTree,
}

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

/// The serialized anchor's `hints` subtree never holds a gitlink (mode
/// `160000`) anywhere in it — every fingerprint and descriptor is an
/// ordinary blob or tree entry.
#[test]
fn hints_never_embed_a_gitlink() {
    let dir = fixture_repo(&numbered(1..=10));
    let repo = gix::open(dir.path()).unwrap();
    let anchor = gix_anchor::capture(
        &repo,
        "HEAD",
        "file.txt",
        Some(LineRange { start: 3, end: 4 }),
    )
    .unwrap();
    assert!(!anchor.hints.fingerprints.is_empty());

    let store = ObjectStore::default();
    let root = serialize_into(&anchor, &store).expect("serialize");
    let top = store.get_tree(&root).expect("anchor tree");
    let hints_oid = top
        .iter()
        .find(|e| e.filename == "hints")
        .expect("hints entry")
        .oid;

    let mut stack = vec![hints_oid];
    while let Some(tree) = stack.pop() {
        for entry in store.get_tree(&tree).expect("tree") {
            assert_ne!(
                entry.mode.kind(),
                EntryKind::Commit,
                "a gitlink retains nothing (anchor.retention): {:?}",
                entry.filename
            );
            if entry.mode.kind() == EntryKind::Tree {
                stack.push(entry.oid);
            }
        }
    }
}

/// The anchor's hints stay reachable from the storing document's own tree:
/// walking the comment's tree (the shape `refs/meta/comments/*` points at)
/// reaches every hint object, so the ref keeps them alive through
/// force-push, branch deletion, and gc with no special-casing.
#[test]
fn hints_are_reachable_from_the_storing_documents_tree() {
    let dir = fixture_repo(&numbered(1..=10));
    let repo = gix::open(dir.path()).unwrap();
    let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).unwrap();

    let store = ObjectStore::default();
    let anchor_tree = serialize_into(&anchor, &store).expect("serialize anchor");
    let hints_tree = store
        .get_tree(&anchor_tree)
        .expect("anchor tree")
        .into_iter()
        .find(|entry| entry.filename == "hints")
        .expect("hints entry")
        .oid;

    let comment = Comment {
        body: "anchored".to_owned(),
        anchor: RawTree::new(anchor_tree),
    };
    let root = serialize_into(&comment, &store).expect("serialize comment");

    let mut stack = vec![root];
    let mut found = false;
    while let Some(tree) = stack.pop() {
        for entry in store.get_tree(&tree).expect("tree") {
            if entry.oid == hints_tree {
                found = true;
            }
            if entry.mode.kind() == EntryKind::Tree {
                stack.push(entry.oid);
            }
        }
    }
    assert!(
        found,
        "the hints subtree must be reachable from the comment's own tree"
    );
}

/// A captured anchor round-trips through its tree unchanged — the struct is
/// the schema, and non-ASCII content fingerprints identically either way.
#[test]
fn anchor_round_trips_through_its_tree() {
    let dir = fixture_repo("line 1\nline 2\n\u{fe}\u{ff} non-ascii bytes\n");
    let repo = gix::open(dir.path()).unwrap();
    for lines in [None, Some(LineRange { start: 2, end: 3 })] {
        let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", lines).unwrap();
        let store = ObjectStore::default();
        let root = serialize_into(&anchor, &store).unwrap();
        let back: gix_anchor::Anchor = facet_git_tree::deserialize(&root, &store).unwrap();
        assert_eq!(back, anchor);
    }
}
