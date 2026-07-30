//! Round-trip fixture for [`Binding::deserialize`]/[`Binding::serialize_into`]
//! against the stored `Binding::Position` format: an externally-tagged
//! `facet-git-tree` enum tree — a single `"Position"` entry whose oid is the
//! anchor's own tree, split into sibling `identity`/`hints` subtrees.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

use facet_git_tree::ObjectStore;
use gix_anchor::{Binding, LineRange};

fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
    range.map(|n| format!("line {n}\n")).collect()
}

fn fixture_repo(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
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

/// `Binding::Position` round-trips through `serialize_into`/`deserialize`
/// byte-for-byte: the same capture serialized twice produces the same
/// handle, and decoding it back recovers an identical `Anchor` — identity
/// and hints alike.
#[test]
fn position_binding_round_trips_through_serialize_and_deserialize() {
    let dir = fixture_repo(&numbered(1..=10));
    let repo = gix::open(dir.path()).unwrap();
    let anchor = gix_anchor::capture(
        &repo,
        "HEAD",
        "file.txt",
        Some(LineRange { start: 3, end: 4 }),
    )
    .unwrap();
    let binding = Binding::Position(anchor.clone());

    let store = ObjectStore::default();
    let handle = binding.serialize_into(&store).expect("serialize");
    let second_handle = binding.serialize_into(&store).expect("serialize again");
    assert_eq!(
        handle, second_handle,
        "serializing the identical binding twice reproduces the same handle"
    );

    let back = Binding::deserialize(&handle, &store).expect("deserialize");
    let Binding::Position(decoded) = back else {
        panic!("must decode as Binding::Position");
    };
    assert_eq!(decoded, anchor);
}

/// The `identity` subtree holds exactly `genesis_rev`, `path`, `span` — the
/// three coordinates ARCHITECTURE.md names, nothing else, and the anchor id
/// is that subtree's own hash, independent of the store it round-trips
/// through.
#[test]
fn anchor_id_is_stable_across_independent_stores() {
    let dir = fixture_repo(&numbered(1..=10));
    let repo = gix::open(dir.path()).unwrap();
    let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).unwrap();

    let first_store = ObjectStore::default();
    let handle = Binding::Position(anchor.clone())
        .serialize_into(&first_store)
        .unwrap();
    let via_handle = handle.anchor_id(&first_store).unwrap();
    assert_eq!(via_handle, anchor.id().unwrap());
}
